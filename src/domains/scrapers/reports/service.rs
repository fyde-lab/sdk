use async_trait::async_trait;

use crate::Result;

use super::Service;
use super::models::Report;
use super::storage::Storage;

/// The default [`Service`] implementation, delegating persistence to an
/// injected [`Storage`].
pub(super) struct ReportsClient<S: Storage> {
    storage: S,
}

impl<S: Storage> ReportsClient<S> {
    pub(super) fn new(storage: S) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl<S: Storage> Service for ReportsClient<S> {
    async fn save(&self, report: Report) -> Result<()> {
        self.storage.save(&report).await
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::FakeReport;
    use super::super::storage::MockStorage;
    use super::*;

    #[tokio::test]
    async fn save_passes_the_report_to_storage() {
        let report = FakeReport::new().with_scraper_name("didaxis").build();

        let mut storage = MockStorage::new();
        let expected = report.clone();
        storage
            .expect_save()
            .withf(move |saved| *saved == expected)
            .times(1)
            .returning(|_| Ok(()));
        let client = ReportsClient::new(storage);

        client.save(report).await.unwrap();
    }
}
