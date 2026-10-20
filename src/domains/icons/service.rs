use async_trait::async_trait;
use uuid::Uuid;

use crate::{ErrorContext as _, Result};

use super::grpc_client::FydeClient;
use super::storage::Storage;
use super::{Icon, Service};

/// The default [`Service`] implementation: a read-through cache in front of
/// the injected `grpc` client, keeping every icon it sees in `storage`.
/// Since an icon never changes once created, a cached icon is served as is,
/// never revalidated.
pub(super) struct IconsClient<S: Storage> {
    grpc: Box<dyn FydeClient>,
    storage: S,
}

impl<S: Storage> IconsClient<S> {
    pub(super) fn new(grpc: impl FydeClient + 'static, storage: S) -> Self {
        Self {
            grpc: Box::new(grpc),
            storage,
        }
    }
}

#[async_trait]
impl<S: Storage> Service for IconsClient<S> {
    async fn create_icon(&self, content: Vec<u8>) -> Result<Icon> {
        let icon = self.grpc.create_icon(content).await?;

        // The creator is the first to show it: cache it right away rather
        // than downloading back what was just uploaded.
        self.storage
            .save(&icon)
            .await
            .with_context(|| format!("failed to cache created icon {}", icon.id))?;

        Ok(icon)
    }

    async fn fetch_icon(&self, id: Uuid) -> Result<Icon> {
        if let Some(icon) = self
            .storage
            .get(id)
            .await
            .with_context(|| format!("failed to look up icon {id} in the cache"))?
        {
            return Ok(icon);
        }

        let icon = self.grpc.fetch_icon(id).await?;
        self.storage
            .save(&icon)
            .await
            .with_context(|| format!("failed to cache fetched icon {id}"))?;

        Ok(icon)
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate::eq;

    use super::*;
    use crate::domains::icons::FakeIcon;
    use crate::domains::icons::grpc_client::MockFydeClient;
    use crate::domains::icons::storage::MockStorage;

    #[tokio::test]
    async fn create_icon_uploads_then_caches_the_icon() {
        let icon = FakeIcon::new().with_content(vec![1, 2, 3]).build();
        let mut grpc = MockFydeClient::new();
        let returned = icon.clone();
        grpc.expect_create_icon()
            .with(eq(vec![1, 2, 3]))
            .times(1)
            .return_once(move |_| Ok(returned));
        let mut storage = MockStorage::new();
        let cached = icon.clone();
        storage
            .expect_save()
            .withf(move |saved| *saved == cached)
            .times(1)
            .returning(|_| Ok(()));

        let created = IconsClient::new(grpc, storage)
            .create_icon(vec![1, 2, 3])
            .await
            .unwrap();

        assert_eq!(created, icon);
    }

    #[tokio::test]
    async fn fetch_icon_serves_a_cached_icon_without_asking_the_server() {
        let icon = FakeIcon::new().build();
        let mut storage = MockStorage::new();
        let cached = icon.clone();
        storage
            .expect_get()
            .with(eq(icon.id()))
            .times(1)
            .return_once(move |_| Ok(Some(cached)));
        // No expectation on the gRPC mock: any call panics.

        let fetched = IconsClient::new(MockFydeClient::new(), storage)
            .fetch_icon(icon.id())
            .await
            .unwrap();

        assert_eq!(fetched, icon);
    }

    #[tokio::test]
    async fn fetch_icon_fetches_and_caches_an_icon_missing_from_the_cache() {
        let icon = FakeIcon::new().build();
        let mut storage = MockStorage::new();
        storage
            .expect_get()
            .with(eq(icon.id()))
            .times(1)
            .return_once(|_| Ok(None));
        let cached = icon.clone();
        storage
            .expect_save()
            .withf(move |saved| *saved == cached)
            .times(1)
            .returning(|_| Ok(()));
        let mut grpc = MockFydeClient::new();
        let returned = icon.clone();
        grpc.expect_fetch_icon()
            .with(eq(icon.id()))
            .times(1)
            .return_once(move |_| Ok(returned));

        let fetched = IconsClient::new(grpc, storage)
            .fetch_icon(icon.id())
            .await
            .unwrap();

        assert_eq!(fetched, icon);
    }

    #[tokio::test]
    async fn fetch_icon_caches_nothing_when_the_server_fails() {
        let id = Uuid::new_v4();
        let mut storage = MockStorage::new();
        storage.expect_get().times(1).return_once(|_| Ok(None));
        storage.expect_save().never();
        let mut grpc = MockFydeClient::new();
        grpc.expect_fetch_icon()
            .times(1)
            .return_once(|_| Err(crate::ErrorKind::InvalidResponse("boom".to_string()).into()));

        let result = IconsClient::new(grpc, storage).fetch_icon(id).await;

        assert!(result.is_err());
    }
}
