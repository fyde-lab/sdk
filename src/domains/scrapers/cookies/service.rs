use async_trait::async_trait;

use crate::Result;

use super::Service;
use super::models::Cookie;
use super::storage::Storage;

/// The default [`Service`] implementation, delegating persistence to an
/// injected [`Storage`].
pub(super) struct CookiesClient<S: Storage> {
    storage: S,
}

impl<S: Storage> CookiesClient<S> {
    pub(super) fn new(storage: S) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl<S: Storage> Service for CookiesClient<S> {
    async fn load(&self, scraper_name: &str) -> Result<Vec<Cookie>> {
        self.storage.list(scraper_name).await
    }

    async fn save(&self, scraper_name: &str, cookies: Vec<Cookie>) -> Result<()> {
        self.storage.replace_all(scraper_name, cookies).await
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate::eq;

    use super::super::models::FakeCookie;
    use super::super::storage::MockStorage;
    use super::*;

    #[tokio::test]
    async fn load_returns_the_storages_cookies() {
        let cookie = FakeCookie::new().with_origin("https://example.com").build();

        let mut storage = MockStorage::new();
        let expected = vec![cookie.clone()];
        storage
            .expect_list()
            .with(eq("didaxis"))
            .times(1)
            .returning(move |_| Ok(expected.clone()));
        let client = CookiesClient::new(storage);

        assert_eq!(client.load("didaxis").await.unwrap(), vec![cookie]);
    }

    #[tokio::test]
    async fn save_replaces_the_scrapers_cookies_in_storage() {
        let cookies = vec![FakeCookie::new().build(), FakeCookie::new().build()];

        let mut storage = MockStorage::new();
        let expected = cookies.clone();
        storage
            .expect_replace_all()
            .withf(move |scraper_name, saved| scraper_name == "didaxis" && *saved == expected)
            .times(1)
            .returning(|_, _| Ok(()));
        let client = CookiesClient::new(storage);

        client.save("didaxis", cookies).await.unwrap();
    }
}
