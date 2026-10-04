use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::{ErrorContext as _, Result};

use super::models::Cookie;
use super::storage::Storage;

/// A [`Storage`] backed by the SDK's local SQLite database (the
/// `scraper_cookies` table).
pub(crate) struct SqliteStorage {
    pool: SqlitePool,
}

impl SqliteStorage {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl Storage for SqliteStorage {
    async fn list(&self, scraper_name: &str) -> Result<Vec<Cookie>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT origin, set_cookie FROM scraper_cookies WHERE scraper_name = ?1",
        )
        .bind(scraper_name)
        .fetch_all(&self.pool)
        .await
        .with_context(|| {
            format!("failed to fetch cookies for scraper {scraper_name:?} from local database")
        })?;

        Ok(rows
            .into_iter()
            .map(|(origin, set_cookie)| Cookie { origin, set_cookie })
            .collect())
    }

    async fn replace_all(&self, scraper_name: &str, cookies: Vec<Cookie>) -> Result<()> {
        let mut tx = self.pool.begin().await.with_context(|| {
            format!("failed to start a transaction to save cookies for scraper {scraper_name:?}")
        })?;

        sqlx::query("DELETE FROM scraper_cookies WHERE scraper_name = ?1")
            .bind(scraper_name)
            .execute(&mut *tx)
            .await
            .with_context(|| {
                format!("failed to clear previous cookies for scraper {scraper_name:?}")
            })?;

        for cookie in &cookies {
            sqlx::query(
                "INSERT INTO scraper_cookies (scraper_name, origin, set_cookie) VALUES (?1, ?2, ?3)",
            )
            .bind(scraper_name)
            .bind(&cookie.origin)
            .bind(&cookie.set_cookie)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("failed to save a cookie for scraper {scraper_name:?}"))?;
        }

        tx.commit().await.with_context(|| {
            format!("failed to commit saved cookies for scraper {scraper_name:?}")
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::super::models::FakeCookie;
    use super::*;

    async fn setup() -> SqliteStorage {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        sqlx::migrate!().run(&pool).await.unwrap();

        SqliteStorage::new(pool)
    }

    #[tokio::test]
    async fn list_returns_empty_when_never_saved() {
        let storage = setup().await;

        assert_eq!(storage.list("didaxis").await.unwrap(), Vec::new());
    }

    #[tokio::test]
    async fn replace_all_then_list_returns_the_saved_cookies() {
        let storage = setup().await;
        let cookies = vec![
            FakeCookie::new()
                .with_origin("https://a.example.com")
                .build(),
            FakeCookie::new()
                .with_origin("https://b.example.com")
                .build(),
        ];

        storage
            .replace_all("didaxis", cookies.clone())
            .await
            .unwrap();

        let mut saved = storage.list("didaxis").await.unwrap();
        saved.sort_by(|a, b| a.origin.cmp(&b.origin));
        let mut expected = cookies;
        expected.sort_by(|a, b| a.origin.cmp(&b.origin));
        assert_eq!(saved, expected);
    }

    #[tokio::test]
    async fn replace_all_overwrites_previously_saved_cookies() {
        let storage = setup().await;
        storage
            .replace_all(
                "didaxis",
                vec![
                    FakeCookie::new()
                        .with_origin("https://old.example.com")
                        .build(),
                ],
            )
            .await
            .unwrap();

        let new_cookie = FakeCookie::new()
            .with_origin("https://new.example.com")
            .build();
        storage
            .replace_all("didaxis", vec![new_cookie.clone()])
            .await
            .unwrap();

        assert_eq!(storage.list("didaxis").await.unwrap(), vec![new_cookie]);
    }

    #[tokio::test]
    async fn distinct_scrapers_are_stored_independently() {
        let storage = setup().await;
        let didaxis_cookie = FakeCookie::new()
            .with_origin("https://didaxis.example.com")
            .build();
        let impots_cookie = FakeCookie::new()
            .with_origin("https://impots.example.com")
            .build();

        storage
            .replace_all("didaxis", vec![didaxis_cookie.clone()])
            .await
            .unwrap();
        storage
            .replace_all("impots", vec![impots_cookie.clone()])
            .await
            .unwrap();

        assert_eq!(storage.list("didaxis").await.unwrap(), vec![didaxis_cookie]);
        assert_eq!(storage.list("impots").await.unwrap(), vec![impots_cookie]);
    }
}
