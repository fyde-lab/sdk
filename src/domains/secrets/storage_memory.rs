use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use zeroize::Zeroizing;

use crate::Result;

use super::storage::Storage;

/// A [`Storage`] kept in process memory only, for clients backed by an
/// in-memory database (`Storage::Memory`), which must never leave anything
/// behind in the OS's credential store. Values are wiped from memory when
/// replaced, removed or dropped.
#[derive(Default)]
pub(super) struct MemoryStorage {
    values: Mutex<HashMap<String, Zeroizing<String>>>,
}

impl MemoryStorage {
    fn values(&self) -> std::sync::MutexGuard<'_, HashMap<String, Zeroizing<String>>> {
        self.values.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[async_trait]
impl Storage for MemoryStorage {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.values().get(key).map(|value| value.to_string()))
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        self.values()
            .insert(key.to_string(), Zeroizing::new(value.to_string()));
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.values().remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn get_returns_what_set_stored() {
        let storage = MemoryStorage::default();

        storage.set("key", "value").await.unwrap();

        assert_eq!(storage.get("key").await.unwrap(), Some("value".to_string()));
    }

    #[tokio::test]
    async fn delete_removes_the_value_and_tolerates_a_missing_key() {
        let storage = MemoryStorage::default();
        storage.set("key", "value").await.unwrap();

        storage.delete("key").await.unwrap();
        storage.delete("key").await.unwrap();

        assert_eq!(storage.get("key").await.unwrap(), None);
    }
}
