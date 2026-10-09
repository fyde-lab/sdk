use crate::domains::settings::Service as SettingsService;
use crate::{ErrorContext as _, Result};

use super::{ALL_SECRETS, Service};

/// Moves every secret an earlier version of this SDK kept in the plaintext
/// `settings` table (under the same key it now has in [`Service`]) into
/// `secrets`, deleting it from `settings` once it's safely stored — so a
/// device upgraded with a session already open stays logged in, and its
/// master key stops sitting in the local database. A no-op once nothing is
/// left to move, so it's safe to run on every `Client::init`. The local
/// database runs with `secure_delete` on (see `sql::SqliteClient`), so the
/// deleted rows are overwritten rather than just unlinked.
pub(crate) async fn migrate_from_settings(
    secrets: &dyn Service,
    settings: &dyn SettingsService,
) -> Result<()> {
    for key in ALL_SECRETS {
        let Some(value) = settings
            .get(key)
            .await
            .with_context(|| format!("failed to read legacy {key:?} from settings"))?
        else {
            continue;
        };

        secrets
            .set(key, &value)
            .await
            .with_context(|| format!("failed to move legacy {key:?} into the credential store"))?;
        settings
            .delete(key)
            .await
            .with_context(|| format!("failed to delete legacy {key:?} from settings"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use mockall::predicate::eq;

    use super::super::{
        MASTER_KEY_SECRET, MockService as MockSecretsService, SESSION_TOKEN_SECRET,
    };
    use super::*;
    use crate::Error;
    use crate::domains::settings::MockService as MockSettingsService;

    #[tokio::test]
    async fn moves_each_legacy_secret_out_of_settings() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .with(eq(MASTER_KEY_SECRET))
            .returning(|_| Ok(Some("the-key".to_string())));
        settings
            .expect_get()
            .with(eq(SESSION_TOKEN_SECRET))
            .returning(|_| Ok(Some("a-token".to_string())));
        settings
            .expect_delete()
            .with(eq(MASTER_KEY_SECRET))
            .times(1)
            .returning(|_| Ok(()));
        settings
            .expect_delete()
            .with(eq(SESSION_TOKEN_SECRET))
            .times(1)
            .returning(|_| Ok(()));

        let mut secrets = MockSecretsService::new();
        secrets
            .expect_set()
            .with(eq(MASTER_KEY_SECRET), eq("the-key"))
            .times(1)
            .returning(|_, _| Ok(()));
        secrets
            .expect_set()
            .with(eq(SESSION_TOKEN_SECRET), eq("a-token"))
            .times(1)
            .returning(|_, _| Ok(()));

        migrate_from_settings(&secrets, &settings).await.unwrap();
    }

    #[tokio::test]
    async fn is_a_no_op_when_nothing_is_left_in_settings() {
        let mut settings = MockSettingsService::new();
        settings.expect_get().returning(|_| Ok(None));
        // No `expect_set()`/`expect_delete()`: the mocks panic if called.

        migrate_from_settings(&MockSecretsService::new(), &settings)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn keeps_the_legacy_copy_if_the_credential_store_rejects_it() {
        let mut settings = MockSettingsService::new();
        settings
            .expect_get()
            .with(eq(MASTER_KEY_SECRET))
            .returning(|_| Ok(Some("the-key".to_string())));
        // No `expect_delete()`: deleting before the secret is safely stored
        // elsewhere would lose the account's only local master key.

        let mut secrets = MockSecretsService::new();
        secrets
            .expect_set()
            .returning(|_, _| Err(Error::Encryption("keychain locked".into())));

        assert!(migrate_from_settings(&secrets, &settings).await.is_err());
    }
}
