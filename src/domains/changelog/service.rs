use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use futures::StreamExt;
use sha2::{Digest as _, Sha256};
use tokio::sync::watch;
use uuid::Uuid;

use crate::domains::documents::{Document, Indexer, Metadata, Storage as DocumentStorage};
use crate::domains::scrapers::SessionStorage as ScraperSessionStorage;
use crate::domains::scripts::{InstalledScript, Storage as ScriptStorage};
use crate::domains::secrets::Service as SecretsService;
use crate::domains::server_state::Service as ServerStateService;
use crate::{Error, ErrorContext as _, ErrorKind, Result};

use super::Service;
use super::crypto;
use super::grpc_client::{ChangelogEvent as ProtoChangelogEvent, FydeClient};
use super::models::{ChangelogEvent, EventType};
use super::storage::CursorStorage;

impl ChangelogEvent {
    /// Decrypts a proto event received from the server into its domain
    /// representation. Not a `TryFrom` impl since decryption needs an
    /// async round trip to `secrets` to derive the KEK (see
    /// [`crypto::decrypt_event`]).
    async fn from_proto(proto: ProtoChangelogEvent, secrets: &dyn SecretsService) -> Result<Self> {
        let id = Uuid::parse_str(&proto.id).with_context(|| {
            format!(
                "invalid changelog event id {:?} returned by server",
                proto.id
            )
        })?;

        let (event_type, subject_id, content, metadata) =
            crypto::decrypt_event(secrets, &proto.encrypted_content)
                .await
                .with_context(|| format!("failed to decrypt changelog event {id}"))?;

        Ok(Self {
            id,
            event_type,
            subject_id,
            content,
            metadata,
        })
    }
}

/// Checks that `proto` directly follows `offset`, the last event consumed
/// (`None` before the account's first): the server chains each of a user's
/// events to that user's previous one through `previous_id` (empty for their
/// first). Any other `previous_id` means an event is missing in between, or
/// that this one isn't this account's — applying it, and moving the cursor
/// past the gap, would lose the missing event for good. Checked before
/// decrypting, so a foreign event fails as such rather than as undecryptable.
fn check_follows(proto: &ProtoChangelogEvent, offset: Option<Uuid>) -> Result<()> {
    let previous_id = if proto.previous_id.is_empty() {
        None
    } else {
        Some(Uuid::parse_str(&proto.previous_id).with_context(|| {
            format!(
                "invalid previous id {:?} on changelog event {:?} returned by server",
                proto.previous_id, proto.id
            )
        })?)
    };

    if previous_id != offset {
        return Err(ErrorKind::InvalidChangelogEvent(format!(
            "changelog event {} follows {previous_id:?}, not {offset:?}, the last event \
             consumed: an event is missing or this one isn't this account's",
            proto.id
        ))
        .into());
    }

    Ok(())
}

/// Delay between reachability checks in [`ChangelogClient::consume_once`]
/// while waiting for the server to come back up.
const SERVER_REACHABILITY_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(200);

/// How long [`ChangelogClient::run_consume_loop`] waits before retrying
/// after a failed pass, doubled after each further failure up to
/// [`CONSUME_RETRY_MAX_DELAY`]. An error that won't go away by itself (an
/// event this device can't decrypt, say) would otherwise be retried, and
/// logged, as fast as the server answers.
const CONSUME_RETRY_INITIAL_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// The longest [`ChangelogClient::run_consume_loop`] waits between retries.
const CONSUME_RETRY_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

/// How long [`ChangelogClient::run_consume_loop`] waits before reopening a
/// stream that ended cleanly. The server keeps it open while the session
/// lasts, so a clean end is rare (a restart, a proxy's idle timeout); without
/// a pause, a server or proxy that ends every stream at once would have this
/// device reconnect as fast as it answers.
const CONSUME_RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// After failing this many passes in a row, an event is reported as
/// blocking sync. It's still retried, never skipped: the cursor never moves
/// past an event that wasn't applied.
const POISON_PILL_PASSES: u32 = 5;

/// A client for the fyde server's changelog service. Generic over the
/// [`DocumentStorage`] implementation used by [`Service::consume`] to spot
/// documents already cached, the [`ScriptStorage`]
/// implementation it saves installed scripts to, the
/// [`ScraperSessionStorage`] it clears a scraper's session in when a new
/// version of its script is installed, and the [`CursorStorage`]
/// implementation used to track how far the changelog has been consumed.
pub(super) struct ChangelogClient<
    D: DocumentStorage,
    S: ScriptStorage,
    P: ScraperSessionStorage,
    O: CursorStorage,
> {
    grpc: Box<dyn FydeClient>,
    document_storage: D,
    indexer: Arc<dyn Indexer>,
    script_storage: S,
    scraper_session_storage: P,
    cursor_storage: O,
    server_state: Arc<dyn ServerStateService>,
    secrets: Arc<dyn SecretsService>,
    /// Signals [`Service::stop_consume_job`] to the task spawned by
    /// [`Service::start_consume_job`]. A `watch` channel (rather than
    /// [`tokio::sync::Notify`]) so the signal is level-triggered: a
    /// `stop_consume_job()` call is never lost even if it lands before the
    /// task starts watching it or between two of its passes.
    stop: watch::Sender<bool>,
    /// The event the last passes failed on (by its id as the server sent
    /// it), and how many passes in a row did.
    failure_streak: Mutex<Option<(String, u32)>>,
}

impl<D: DocumentStorage, S: ScriptStorage, P: ScraperSessionStorage, O: CursorStorage>
    ChangelogClient<D, S, P, O>
{
    /// Creates a client for the changelog service over `grpc`, using
    /// `document_storage` to spot documents [`Service::consume`] already
    /// cached, `indexer` to cache (and index) the ones it materializes,
    /// `script_storage` to save the scripts it
    /// materializes, `scraper_session_storage` to clear the saved session
    /// of a scraper whose script changes version, `cursor_storage` to track its progress through the
    /// changelog, `server_state` to wait for the server to be reachable
    /// before opening a changelog stream, `secrets` to read the account
    /// master key [`crypto`] derives every event's KEK from. Assumes it's
    /// the only one writing that local storage: `Client::init`'s instance
    /// lock guarantees no other client uses the same database.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        grpc: impl FydeClient + 'static,
        document_storage: D,
        indexer: Arc<dyn Indexer>,
        script_storage: S,
        scraper_session_storage: P,
        cursor_storage: O,
        server_state: Arc<dyn ServerStateService>,
        secrets: Arc<dyn SecretsService>,
    ) -> Self {
        Self {
            grpc: Box::new(grpc),
            document_storage,
            indexer,
            script_storage,
            scraper_session_storage,
            cursor_storage,
            server_state,
            secrets,
            stop: watch::Sender::new(false),
            failure_streak: Mutex::new(None),
        }
    }

    /// Counts `err` as one more pass failing on the event `event_id`,
    /// reporting the event as blocking sync once it has failed
    /// [`POISON_PILL_PASSES`] in a row, and returns `err`.
    fn event_failed(&self, event_id: &str, err: Error) -> Error {
        let mut streak = self
            .failure_streak
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let passes = match streak.as_mut() {
            Some((id, passes)) if id == event_id => {
                *passes += 1;
                *passes
            }
            _ => {
                *streak = Some((event_id.to_string(), 1));
                1
            }
        };
        if passes >= POISON_PILL_PASSES {
            tracing::error!(
                event_id,
                passes,
                "changelog event {event_id} has failed {passes} passes in a row, blocking sync \
                 behind it: {err}"
            );
        }

        err
    }

    fn event_applied(&self) {
        *self
            .failure_streak
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Checks that `proto` follows `offset` (see [`check_follows`]) and
    /// decrypts it, along with the digest of its ciphertext.
    async fn decode(
        &self,
        proto: ProtoChangelogEvent,
        offset: Option<Uuid>,
    ) -> Result<(ChangelogEvent, [u8; 32])> {
        check_follows(&proto, offset)?;
        let digest: [u8; 32] = Sha256::digest(&proto.encrypted_content).into();
        let event = ChangelogEvent::from_proto(proto, &*self.secrets).await?;

        Ok((event, digest))
    }

    /// Applies `event` to local storage: caches (and indexes) the document
    /// it saves or updates, or saves the script it installs.
    async fn apply(&self, event: &ChangelogEvent) -> Result<()> {
        match event.event_type {
            EventType::SaveDocument => {
                let (Some(content), Some(metadata)) = (&event.content, &event.metadata) else {
                    return Err(ErrorKind::InvalidChangelogEvent(format!(
                        "created event {} for document {} is missing content or metadata",
                        event.id, event.subject_id
                    ))
                    .into());
                };

                // `upload` only rejects a duplicate already cached
                // here, so two uploads of the same content that
                // haven't synced yet (or from two devices) both get
                // published: keep the first one cached, skip the
                // rest. Documents from before checksums existed
                // have an empty one, which matches nothing.
                let duplicate_of = if metadata.checksum.is_empty() {
                    None
                } else {
                    self.document_storage
                        .find_document_by_checksum(&metadata.checksum)
                        .await
                        .with_context(|| {
                            format!(
                                "failed to look up local documents with the same content as document {}",
                                event.subject_id
                            )
                        })?
                };

                if let Some(existing) = duplicate_of {
                    tracing::warn!(
                        event_id = %event.id,
                        document_id = %event.subject_id,
                        existing_document_id = %existing,
                        "skipping caching document {}: a document with the same content is already cached locally as {existing}",
                        event.subject_id
                    );
                    self.indexer.skip_document(event.subject_id);
                    return Ok(());
                }

                let document = Document::new(event.subject_id, content.clone(), metadata.clone());
                self.indexer
                    .save_document(&document)
                    .await
                    .with_context(|| format!("failed to apply changelog event {}", event.id))
            }
            EventType::UpdateMetadata => {
                let Some(metadata) = &event.metadata else {
                    return Err(ErrorKind::InvalidChangelogEvent(format!(
                        "update metadata event {} for document {} is missing metadata",
                        event.id, event.subject_id
                    ))
                    .into());
                };

                self.indexer
                    .update_metadata(event.subject_id, metadata)
                    .await
                    .with_context(|| format!("failed to apply changelog event {}", event.id))
            }
            EventType::Deleted => Ok(()),
            EventType::InstallScript => {
                let Some(content) = &event.content else {
                    return Err(ErrorKind::InvalidChangelogEvent(format!(
                        "script installed event {} for script {} is missing content",
                        event.id, event.subject_id
                    ))
                    .into());
                };
                let installed: InstalledScript =
                    serde_json::from_slice(content).map_err(|err| {
                        ErrorKind::InvalidChangelogEvent(format!(
                            "script installed event {} for script {} carries an invalid \
                         installed script: {err}",
                            event.id, event.subject_id
                        ))
                    })?;

                self.reset_scraper_session_on_new_version(&installed)
                    .await?;
                self.script_storage
                    .save_installed_script(&installed)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to save installed script {} locally",
                            event.subject_id
                        )
                    })
            }
        }
    }

    /// Forgets the scraper session saved under `installed`'s script id (the
    /// name [`crate::domains::scrapers::Service::run_installed`] keeps it
    /// under) unless the installation it replaces has the same script
    /// version: a new version must not trust what an older one recorded
    /// there, such as the URLs `fyde.helpers.save_pdf` skips as already
    /// saved, so its next run fetches every document again. Reinstalling the
    /// same version (e.g. with new credentials) keeps the session.
    async fn reset_scraper_session_on_new_version(
        &self,
        installed: &InstalledScript,
    ) -> Result<()> {
        let script = installed.script();
        let previous = self
            .script_storage
            .find_installed_script(script.id())
            .await
            .with_context(|| {
                format!(
                    "failed to look up the installed version of script {}",
                    script.id()
                )
            })?;
        if previous.is_some_and(|previous| previous.script().version() == script.version()) {
            return Ok(());
        }

        self.scraper_session_storage
            .delete(&script.id().to_string())
            .await
            .with_context(|| {
                format!(
                    "failed to clear the scraper session of script {} for its version {}",
                    script.id(),
                    script.version()
                )
            })
    }

    /// One pass of the consume loop spawned by [`Service::start_consume_job`]:
    /// opens a stream from the persisted
    /// cursor and processes events until the stream ends or an error
    /// occurs. Split out as an inherent method (rather than inlined in
    /// `consume`) so it can be retried in a loop without re-implementing
    /// the retry itself, and so tests can exercise a single pass directly.
    async fn consume_once(&self, callback: &mut (dyn FnMut(ChangelogEvent) + Send)) -> Result<()> {
        let cursor = self
            .cursor_storage
            .get_cursor()
            .await
            .context("failed to read the local changelog cursor")?;

        let since = if cursor.is_nil() { None } else { Some(cursor) };

        while !self
            .server_state
            .is_server_reachable()
            .await
            .context("failed to check server reachability")?
        {
            tokio::time::sleep(SERVER_REACHABILITY_POLL_INTERVAL).await;
        }

        let mut stream = self
            .grpc
            .consume_since(since)
            .await
            .with_context(|| format!("failed to open changelog stream from id {since:?}"))?;

        // The last event this pass went through, starting from the persisted
        // cursor.
        let mut offset = since;

        // Whether the indexing model is loaded: documents are indexed as
        // they're cached, so none is applied until it is.
        let mut indexing_ready = false;

        while let Some(proto_event) = stream.next().await {
            let proto_event = proto_event.context("failed to read next changelog event")?;
            let proto_id = proto_event.id.clone();
            let (event, digest) = self
                .decode(proto_event, offset)
                .await
                .map_err(|err| self.event_failed(&proto_id, err))?;

            // The server can't forge or re-encrypt an event, but it can serve
            // an old one again under a new id, e.g. to roll an installed
            // script or a document's metadata back: an event this database
            // already applied is skipped, byte-identical ciphertext being
            // how a replay is told apart from a genuine new event.
            if self
                .cursor_storage
                .is_applied(&digest)
                .await
                .with_context(|| {
                    format!(
                        "failed to check whether changelog event {} was already applied",
                        event.id
                    )
                })?
            {
                tracing::warn!(
                    event_id = %event.id,
                    subject_id = %event.subject_id,
                    "skipping changelog event {}: it replays an event already applied",
                    event.id
                );
                self.cursor_storage
                    .save_cursor(event.id)
                    .await
                    .with_context(|| format!("failed to persist changelog cursor {}", event.id))?;
                self.event_applied();
                offset = Some(event.id);
                continue;
            }

            // A model that can't be fetched isn't the event's fault, so isn't
            // counted against it.
            if !indexing_ready
                && matches!(
                    event.event_type,
                    EventType::SaveDocument | EventType::UpdateMetadata
                )
            {
                self.indexer.prepare().await.with_context(|| {
                    format!(
                        "cannot apply changelog event {} before the indexing model is available",
                        event.id
                    )
                })?;
                indexing_ready = true;
            }

            self.apply(&event)
                .await
                .map_err(|err| self.event_failed(&proto_id, err))?;
            self.event_applied();

            self.cursor_storage
                .mark_applied(&digest)
                .await
                .with_context(|| {
                    format!("failed to record changelog event {} as applied", event.id)
                })?;
            self.cursor_storage
                .save_cursor(event.id)
                .await
                .with_context(|| format!("failed to persist changelog cursor {}", event.id))?;

            offset = Some(event.id);
            callback(event);
        }

        Ok(())
    }

    /// The retry loop spawned by [`Service::start_consume_job`] onto its own
    /// background task: repeats [`Self::consume_once`] from the persisted
    /// cursor — after [`CONSUME_RECONNECT_DELAY`] after a clean end of
    /// stream, after a growing delay (see [`CONSUME_RETRY_INITIAL_DELAY`])
    /// after an error, which it
    /// logs — until [`Service::stop_consume_job`] signals it to return, even
    /// mid-delay. Split out as its own inherent method (rather than inlined
    /// in `start_consume_job`'s spawned task) so tests can exercise the loop
    /// directly, synchronously, without needing to observe a detached task's
    /// completion.
    async fn run_consume_loop(&self, callback: &mut (dyn FnMut(ChangelogEvent) + Send)) {
        let mut stop = self.stop.subscribe();
        let mut retry_delay = CONSUME_RETRY_INITIAL_DELAY;

        while !*stop.borrow() {
            let delay = tokio::select! {
                result = self.consume_once(callback) => match result {
                    Ok(()) => {
                        retry_delay = CONSUME_RETRY_INITIAL_DELAY;
                        tracing::debug!(
                            "changelog stream ended, reopening it in {CONSUME_RECONNECT_DELAY:?}"
                        );
                        CONSUME_RECONNECT_DELAY
                    }
                    Err(err) => {
                        tracing::error!(
                            "changelog consume failed, retrying in {retry_delay:?}: {err:?}"
                        );
                        let delay = retry_delay;
                        retry_delay = (retry_delay * 2).min(CONSUME_RETRY_MAX_DELAY);
                        delay
                    }
                },
                _ = stop.changed() => continue,
            };
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = stop.changed() => {}
            }
        }
    }
}

#[async_trait]
impl<
    D: DocumentStorage + 'static,
    S: ScriptStorage + 'static,
    P: ScraperSessionStorage + 'static,
    O: CursorStorage + 'static,
> Service for ChangelogClient<D, S, P, O>
{
    async fn ensure_master_key(&self) -> Result<()> {
        crypto::ensure_master_key(&*self.secrets).await
    }

    async fn send(
        &self,
        event_type: EventType,
        subject_id: Uuid,
        content: Option<&[u8]>,
        metadata: Option<&Metadata>,
    ) -> Result<()> {
        let encrypted_content =
            crypto::encrypt_event(&*self.secrets, event_type, subject_id, content, metadata)
                .await
                .context("failed to encrypt changelog event")?;
        if encrypted_content.len() > super::MAX_EVENT_BYTES {
            return Err(ErrorKind::DocumentTooLarge {
                what: format!("the encrypted {event_type:?} event for {subject_id}"),
                size: encrypted_content.len(),
                max: super::MAX_EVENT_BYTES,
            }
            .into());
        }

        self.grpc
            .record_event(encrypted_content)
            .await
            .context("failed to send changelog event to server")?;

        Ok(())
    }

    async fn start_consume_job(
        self: Arc<Self>,
        mut callback: Box<dyn FnMut(ChangelogEvent) + Send>,
    ) -> Result<()> {
        // Reset in case a previous job on this same client was stopped: the
        // `stop` signal is level-triggered (see the field's doc comment)
        // and would otherwise still read as `true` here, making this job
        // return immediately without ever streaming.
        self.stop.send_replace(false);

        tokio::spawn(async move { self.run_consume_loop(&mut *callback).await });

        Ok(())
    }

    fn stop_consume_job(&self) {
        // `send_replace`, not `send`: `send` is a no-op (doesn't even
        // update the value) when there are currently no receivers, which
        // is exactly the case where `stop_consume_job` is called before
        // `start_consume_job`'s task has subscribed — `send_replace`
        // updates the value unconditionally, so a task that starts
        // afterwards still sees it immediately via `subscribe`.
        self.stop.send_replace(true);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::super::grpc_client::MockFydeClient;
    use super::super::storage::MockCursorStorage;
    use super::*;
    use crate::domains::documents::{
        DocumentIndexer, FakeCachedEmbeddings, FakeMetadata, MockEmbedder, MockEmbeddingCache,
        MockIndexer, MockStorage,
    };
    use crate::domains::scrapers::MockSessionStorage;
    use crate::domains::scripts::{
        FakeInstalledScript, FakeScript, MockStorage as MockScriptStorage,
    };
    use crate::domains::secrets::MockService as MockSecretsService;
    use crate::domains::server_state::MockService as MockServerState;

    /// A [`ServerStateService`] mock reporting the server as always
    /// reachable, for tests where reachability isn't under test.
    fn reachable_server_state() -> Arc<dyn ServerStateService> {
        let mut mock = MockServerState::new();
        mock.expect_is_server_reachable().returning(|| Ok(true));
        Arc::new(mock)
    }

    /// A [`SecretsService`] mock with a fixed raw master key set under
    /// [`crate::domains::secrets::MASTER_KEY_SECRET`], for tests
    /// exercising real `crypto::encrypt_event`/`decrypt_event` calls.
    fn fake_secrets() -> Arc<dyn SecretsService> {
        let mut mock = MockSecretsService::new();
        let encoded = crate::domains::users::encode_master_key(b"the-account-master-key")
            .unwrap()
            .to_string();
        mock.expect_get()
            .withf(|key| key == crate::domains::secrets::MASTER_KEY_SECRET)
            .returning(move |_| Ok(Some(encoded.clone())));
        Arc::new(mock)
    }

    /// An indexer with the model loaded, expected to cache nothing.
    fn ready_indexer() -> MockIndexer {
        let mut indexer = MockIndexer::new();
        indexer.expect_prepare().returning(|| Ok(()));
        indexer
    }

    /// An indexer expected to never be called.
    fn idle_indexer() -> Arc<dyn Indexer> {
        Arc::new(MockIndexer::new())
    }

    fn proto_event(id: Uuid, document_id: Uuid) -> ProtoChangelogEvent {
        proto_event_with_type(id, document_id, EventType::SaveDocument)
    }

    fn proto_event_with_type(
        id: Uuid,
        document_id: Uuid,
        event_type: EventType,
    ) -> ProtoChangelogEvent {
        let encrypted_content = futures::executor::block_on(crypto::encrypt_event(
            &*fake_secrets(),
            event_type,
            document_id,
            Some(b"content"),
            Some(&FakeMetadata::new().build()),
        ))
        .unwrap();
        ProtoChangelogEvent {
            id: id.to_string(),
            encrypted_content,
            previous_id: String::new(),
        }
    }

    #[tokio::test]
    async fn consume_decrypts_events_and_caches_the_document() {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_consume_since()
            .withf(|id| id.is_none())
            .returning(move |_| {
                Ok(futures::stream::iter(vec![Ok(proto_event(event_id, document_id))]).boxed())
            });

        let mut document_storage = MockStorage::new();
        document_storage
            .expect_find_document_by_checksum()
            .times(1)
            .returning(|_| Ok(None));
        let mut indexer = ready_indexer();
        indexer
            .expect_save_document()
            .withf(move |document| document.id() == document_id && document.content() == b"content")
            .times(1)
            .returning(|_| Ok(()));

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_is_applied()
            .times(1)
            .returning(|_| Ok(false));
        cursor_storage
            .expect_mark_applied()
            .times(1)
            .returning(|_| Ok(()));
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == event_id)
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::new(
            mock_grpc,
            document_storage,
            Arc::new(indexer),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        let received = std::sync::Arc::new(Mutex::new(Vec::new()));
        let received_in_callback = received.clone();
        let mut callback = move |event| received_in_callback.lock().unwrap().push(event);
        client.consume_once(&mut callback).await.unwrap();

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].id, event_id);
        assert_eq!(received[0].subject_id, document_id);
        assert_eq!(received[0].event_type, EventType::SaveDocument);
    }

    #[tokio::test]
    async fn consume_records_each_applied_event_by_the_digest_of_its_ciphertext() {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();
        let proto = proto_event(event_id, document_id);
        let expected: [u8; 32] = Sha256::digest(&proto.encrypted_content).into();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_consume_since()
            .returning(move |_| Ok(futures::stream::iter(vec![Ok(proto.clone())]).boxed()));

        let mut document_storage = MockStorage::new();
        document_storage
            .expect_find_document_by_checksum()
            .returning(|_| Ok(None));
        let mut indexer = ready_indexer();
        indexer.expect_save_document().returning(|_| Ok(()));

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage
            .expect_is_applied()
            .withf(move |digest| *digest == expected)
            .times(1)
            .returning(|_| Ok(false));
        cursor_storage
            .expect_mark_applied()
            .withf(move |digest| *digest == expected)
            .times(1)
            .returning(|_| Ok(()));
        cursor_storage.expect_save_cursor().returning(|_| Ok(()));

        let client = ChangelogClient::new(
            mock_grpc,
            document_storage,
            Arc::new(indexer),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    /// The embeddings an upload left in the cache are stored with the
    /// document its event carries, never embedded again, and survive a pass
    /// that failed to save it, so the retried pass still reuses them. The
    /// cursor only moves, and the entry is only dropped, once it's saved.
    #[tokio::test]
    async fn consume_reuses_an_uploads_cached_embeddings_even_after_a_failed_pass() {
        let event_id = Uuid::now_v7();
        let metadata = FakeMetadata::new()
            .with_name("Facture mars.pdf")
            .with_transcript("Montant : 42 €")
            .build();
        let document_id = metadata.id;
        let encrypted_content = futures::executor::block_on(crypto::encrypt_event(
            &*fake_secrets(),
            EventType::SaveDocument,
            document_id,
            Some(b"content"),
            Some(&metadata),
        ))
        .unwrap();
        let proto = ProtoChangelogEvent {
            id: event_id.to_string(),
            encrypted_content,
            previous_id: String::new(),
        };

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_consume_since()
            .times(2)
            .returning(move |_| Ok(futures::stream::iter(vec![Ok(proto.clone())]).boxed()));
        let mut document_storage = MockStorage::new();
        document_storage
            .expect_find_document_by_checksum()
            .times(2)
            .returning(|_| Ok(None));

        // No `expect_embed_passages()`: embedding panics.
        let mut embedder = MockEmbedder::new();
        embedder.expect_load().times(2).returning(|| Ok(()));
        embedder.expect_model().return_const("e5.onnx".to_string());

        let cached = Arc::new(
            FakeCachedEmbeddings::new()
                .for_metadata(&metadata, "e5.onnx")
                .build(),
        );
        let mut sequence = mockall::Sequence::new();
        let mut cache = MockEmbeddingCache::new();
        let entry = cached.clone();
        cache
            .expect_get()
            .withf(move |id| *id == document_id)
            .times(2)
            .returning(move |_| Some(entry.clone()));
        let mut indexer_storage = MockStorage::new();
        let expected = cached.embeddings.clone();
        indexer_storage
            .expect_save_document()
            .withf(move |document, embeddings| {
                document.id() == document_id && *embeddings == expected
            })
            .times(1)
            .in_sequence(&mut sequence)
            .returning(
                |_, _| Err(ErrorKind::InvalidChangelogEvent("disk full".to_string()).into()),
            );
        let expected = cached.embeddings.clone();
        indexer_storage
            .expect_save_document()
            .withf(move |_, embeddings| *embeddings == expected)
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Ok(()));
        cache
            .expect_remove()
            .withf(move |id| *id == document_id)
            .times(1)
            .in_sequence(&mut sequence)
            .return_const(());

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage
            .expect_is_applied()
            .times(2)
            .returning(|_| Ok(false));
        cursor_storage
            .expect_mark_applied()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(()));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == event_id)
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(()));

        let client = ChangelogClient::new(
            mock_grpc,
            document_storage,
            Arc::new(DocumentIndexer::new(
                indexer_storage,
                Arc::new(embedder),
                Arc::new(cache),
            )),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        let err = client.consume_once(&mut |_| {}).await.unwrap_err();
        assert!(err.to_string().contains("disk full"), "{err}");

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    #[tokio::test]
    async fn consume_skips_a_replayed_event_but_still_advances_the_cursor() {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(proto_event(event_id, document_id))]).boxed())
        });

        // No expectations set up: the mocks panic if the replay is applied.
        let document_storage = MockStorage::new();

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage
            .expect_is_applied()
            .times(1)
            .returning(|_| Ok(true));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == event_id)
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::new(
            mock_grpc,
            document_storage,
            idle_indexer(),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        let calls = Arc::new(Mutex::new(0));
        let calls_in_callback = calls.clone();
        client
            .consume_once(&mut move |_| *calls_in_callback.lock().unwrap() += 1)
            .await
            .unwrap();

        assert_eq!(*calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn consume_skips_caching_a_document_whose_content_is_already_cached_but_still_calls_back()
    {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();
        let existing = Uuid::now_v7();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(proto_event(event_id, document_id))]).boxed())
        });

        // No `expect_save_document()` set up: the mock panics if the
        // duplicate is cached anyway.
        let mut document_storage = MockStorage::new();
        document_storage
            .expect_find_document_by_checksum()
            .withf(|checksum| !checksum.is_empty())
            .times(1)
            .returning(move |_| Ok(Some(existing)));

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_is_applied()
            .times(1)
            .returning(|_| Ok(false));
        cursor_storage
            .expect_mark_applied()
            .times(1)
            .returning(|_| Ok(()));
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == event_id)
            .times(1)
            .returning(|_| Ok(()));

        // Still told, so it drops the embeddings this upload cached.
        let mut indexer = ready_indexer();
        indexer
            .expect_skip_document()
            .withf(move |id| *id == document_id)
            .times(1)
            .return_const(());

        let client = ChangelogClient::new(
            mock_grpc,
            document_storage,
            Arc::new(indexer),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        let received = std::sync::Arc::new(Mutex::new(Vec::new()));
        let received_in_callback = received.clone();
        let mut callback = move |event| received_in_callback.lock().unwrap().push(event);
        client.consume_once(&mut callback).await.unwrap();

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].subject_id, document_id);
    }

    #[tokio::test(start_paused = true)]
    async fn consume_once_waits_for_the_server_to_become_reachable_before_streaming() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(proto_event(event_id, document_id))]).boxed())
        });

        let mut document_storage = MockStorage::new();
        document_storage
            .expect_find_document_by_checksum()
            .returning(|_| Ok(None));
        let mut indexer = ready_indexer();
        indexer.expect_save_document().returning(|_| Ok(()));

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_is_applied()
            .times(1)
            .returning(|_| Ok(false));
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage.expect_mark_applied().returning(|_| Ok(()));
        cursor_storage.expect_save_cursor().returning(|_| Ok(()));

        let reachability_checks = Arc::new(AtomicU32::new(0));
        let reachability_checks_in_mock = reachability_checks.clone();
        let mut server_state = MockServerState::new();
        server_state
            .expect_is_server_reachable()
            .returning(move || {
                let attempt = reachability_checks_in_mock.fetch_add(1, Ordering::SeqCst);
                Ok(attempt >= 2)
            });

        let client = ChangelogClient::new(
            mock_grpc,
            document_storage,
            Arc::new(indexer),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            Arc::new(server_state),
            fake_secrets(),
        );

        let mut callback = |_| {};
        let consume_once_fut = client.consume_once(&mut callback);
        tokio::pin!(consume_once_fut);

        // Not yet reachable on the first two checks, so the stream must not
        // have been opened yet.
        tokio::time::timeout(std::time::Duration::from_millis(0), &mut consume_once_fut)
            .await
            .unwrap_err();
        assert_eq!(reachability_checks.load(Ordering::SeqCst), 1);

        tokio::time::advance(SERVER_REACHABILITY_POLL_INTERVAL).await;
        tokio::time::timeout(std::time::Duration::from_millis(0), &mut consume_once_fut)
            .await
            .unwrap_err();
        assert_eq!(reachability_checks.load(Ordering::SeqCst), 2);

        // Third check reports reachable, so the pass should now complete.
        tokio::time::advance(SERVER_REACHABILITY_POLL_INTERVAL).await;
        consume_once_fut.await.unwrap();
        assert_eq!(reachability_checks.load(Ordering::SeqCst), 3);
    }

    /// A `InstallScript` proto event for `script_id` carrying `content`
    /// as its (encrypted) payload.
    fn install_script_proto_event(
        id: Uuid,
        script_id: Uuid,
        content: &[u8],
    ) -> ProtoChangelogEvent {
        let encrypted_content = futures::executor::block_on(crypto::encrypt_event(
            &*fake_secrets(),
            EventType::InstallScript,
            script_id,
            Some(content),
            None,
        ))
        .unwrap();
        ProtoChangelogEvent {
            id: id.to_string(),
            encrypted_content,
            previous_id: String::new(),
        }
    }

    /// A cursor storage expecting a single pass from the very beginning of
    /// the changelog that consumes exactly `event_id`.
    fn cursor_storage_consuming(event_id: Uuid) -> MockCursorStorage {
        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_is_applied()
            .times(1)
            .returning(|_| Ok(false));
        cursor_storage
            .expect_mark_applied()
            .times(1)
            .returning(|_| Ok(()));
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == event_id)
            .times(1)
            .returning(|_| Ok(()));
        cursor_storage
    }

    /// Consumes one `InstallScript` event installing `new` over the locally
    /// saved `previous`, with `session_storage` as the scraper sessions.
    async fn consume_reinstall(
        previous: InstalledScript,
        new: InstalledScript,
        session_storage: MockSessionStorage,
    ) {
        let script_id = new.script().id();
        let event_id = Uuid::now_v7();
        let content = serde_json::to_vec(&new).unwrap();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(install_script_proto_event(
                event_id, script_id, &content,
            ))])
            .boxed())
        });

        let mut script_storage = MockScriptStorage::new();
        script_storage
            .expect_find_installed_script()
            .withf(move |id| *id == script_id)
            .times(1)
            .returning(move |_| Ok(Some(previous.clone())));
        script_storage
            .expect_save_installed_script()
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::new(
            mock_grpc,
            MockStorage::new(),
            idle_indexer(),
            script_storage,
            session_storage,
            cursor_storage_consuming(event_id),
            reachable_server_state(),
            fake_secrets(),
        );

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    #[tokio::test]
    async fn consume_clears_the_scraper_session_when_a_new_script_version_is_installed() {
        let script = FakeScript::new().with_version(1).build();
        let script_id = script.id();
        let previous = FakeInstalledScript::new()
            .with_script(script.clone())
            .build();
        let new = FakeInstalledScript::new()
            .with_script(FakeScript::new().with_id(script_id).with_version(2).build())
            .build();

        let mut session_storage = MockSessionStorage::new();
        session_storage
            .expect_delete()
            .withf(move |name| name == script_id.to_string())
            .times(1)
            .returning(|_| Ok(()));

        consume_reinstall(previous, new, session_storage).await;
    }

    #[tokio::test]
    async fn consume_keeps_the_scraper_session_when_the_same_script_version_is_reinstalled() {
        let script = FakeScript::new().with_version(3).build();
        let previous = FakeInstalledScript::new()
            .with_script(script.clone())
            .build();
        let new = FakeInstalledScript::new().with_script(script).build();

        // No `expect_delete`: clearing the session would panic.
        consume_reinstall(previous, new, MockSessionStorage::new()).await;
    }

    #[tokio::test]
    async fn consume_saves_the_installed_script_for_an_install_script_event() {
        let installed = FakeInstalledScript::new().build();
        let script_id = installed.script().id();
        let event_id = Uuid::now_v7();
        let content = serde_json::to_vec(&installed).unwrap();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(install_script_proto_event(
                event_id, script_id, &content,
            ))])
            .boxed())
        });

        let mut script_storage = MockScriptStorage::new();
        script_storage
            .expect_find_installed_script()
            .returning(|_| Ok(None));
        let expected = installed.clone();
        script_storage
            .expect_save_installed_script()
            .withf(move |saved| *saved == expected)
            .times(1)
            .returning(|_| Ok(()));

        let mut session_storage = MockSessionStorage::new();
        session_storage
            .expect_delete()
            .withf(move |name| name == script_id.to_string())
            .times(1)
            .returning(|_| Ok(()));

        // No `MockStorage` expectations: a script event must never touch
        // the documents cache.
        let client = ChangelogClient::new(
            mock_grpc,
            MockStorage::new(),
            idle_indexer(),
            script_storage,
            session_storage,
            cursor_storage_consuming(event_id),
            reachable_server_state(),
            fake_secrets(),
        );

        let received = std::sync::Arc::new(Mutex::new(Vec::new()));
        let received_in_callback = received.clone();
        let mut callback = move |event| received_in_callback.lock().unwrap().push(event);
        client.consume_once(&mut callback).await.unwrap();

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].event_type, EventType::InstallScript);
        assert_eq!(received[0].subject_id, script_id);
    }

    #[tokio::test]
    async fn consume_rejects_an_install_script_event_with_an_invalid_payload() {
        let script_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(install_script_proto_event(
                event_id,
                script_id,
                b"not json",
            ))])
            .boxed())
        });

        // The cursor must not advance past an event that failed to apply.
        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_is_applied()
            .times(1)
            .returning(|_| Ok(false));
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));

        let client = ChangelogClient::new(
            mock_grpc,
            MockStorage::new(),
            idle_indexer(),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        let err = client.consume_once(&mut |_| {}).await.unwrap_err();

        assert!(
            matches!(err.kind(), ErrorKind::InvalidChangelogEvent(_)),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn consume_does_not_cache_the_document_for_a_deleted_event() {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(proto_event_with_type(
                event_id,
                document_id,
                EventType::Deleted,
            ))])
            .boxed())
        });

        // No `expect_save_document()`/`expect_get_document()` set up: the
        // mock panics if either is called, which is how this test proves
        // neither is.
        let document_storage = MockStorage::new();

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_is_applied()
            .times(1)
            .returning(|_| Ok(false));
        cursor_storage
            .expect_mark_applied()
            .times(1)
            .returning(|_| Ok(()));
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == event_id)
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::new(
            mock_grpc,
            document_storage,
            idle_indexer(),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    #[tokio::test]
    async fn consume_updates_the_cached_documents_metadata_for_an_update_metadata_event() {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(proto_event_with_type(
                event_id,
                document_id,
                EventType::UpdateMetadata,
            ))])
            .boxed())
        });

        let document_storage = MockStorage::new();
        let mut indexer = ready_indexer();
        indexer
            .expect_update_metadata()
            .withf(move |id, _metadata| *id == document_id)
            .times(1)
            .returning(|_, _| Ok(()));

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_is_applied()
            .times(1)
            .returning(|_| Ok(false));
        cursor_storage
            .expect_mark_applied()
            .times(1)
            .returning(|_| Ok(()));
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == event_id)
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::new(
            mock_grpc,
            document_storage,
            Arc::new(indexer),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    #[tokio::test]
    async fn consume_errors_on_a_save_document_event_missing_content_or_metadata() {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let encrypted_content = futures::executor::block_on(crypto::encrypt_event(
            &*fake_secrets(),
            EventType::SaveDocument,
            document_id,
            None,
            None,
        ))
        .unwrap();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(ProtoChangelogEvent {
                id: event_id.to_string(),
                encrypted_content: encrypted_content.clone(),
                previous_id: String::new(),
            })])
            .boxed())
        });

        // Decryption fails before either storage is touched, so neither
        // mock needs `save_document`/`save_cursor` expectations: the mock
        // panics if it's called, which is how this test proves it isn't.
        let document_storage = MockStorage::new();
        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_is_applied()
            .times(1)
            .returning(|_| Ok(false));
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));

        let client = ChangelogClient::new(
            mock_grpc,
            document_storage,
            Arc::new(ready_indexer()),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        let result = client.consume_once(&mut |_| {}).await;

        assert!(matches!(
            result.unwrap_err().kind(),
            ErrorKind::InvalidChangelogEvent(_)
        ));
    }

    #[tokio::test]
    async fn consume_resumes_from_the_persisted_cursor() {
        let document_id = Uuid::now_v7();
        let cursor = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_consume_since()
            .withf(move |id| *id == Some(cursor))
            .returning(move |_| {
                let event = ProtoChangelogEvent {
                    previous_id: cursor.to_string(),
                    ..proto_event(event_id, document_id)
                };
                Ok(futures::stream::iter(vec![Ok(event)]).boxed())
            });

        let mut document_storage = MockStorage::new();
        document_storage
            .expect_find_document_by_checksum()
            .returning(|_| Ok(None));
        let mut indexer = ready_indexer();
        indexer.expect_save_document().returning(|_| Ok(()));

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_is_applied()
            .times(1)
            .returning(|_| Ok(false));
        cursor_storage
            .expect_mark_applied()
            .times(1)
            .returning(|_| Ok(()));
        cursor_storage
            .expect_get_cursor()
            .returning(move || Ok(cursor));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == event_id)
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::new(
            mock_grpc,
            document_storage,
            Arc::new(indexer),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    /// A client streaming `events` from the beginning (nil cursor), caching
    /// every `SaveDocument` event that gets that far as a new document.
    fn client_streaming(
        events: Vec<ProtoChangelogEvent>,
    ) -> ChangelogClient<MockStorage, MockScriptStorage, MockSessionStorage, MockCursorStorage>
    {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(events.clone().into_iter().map(Ok)).boxed())
        });

        let mut document_storage = MockStorage::new();
        document_storage
            .expect_find_document_by_checksum()
            .returning(|_| Ok(None));
        let mut indexer = ready_indexer();
        indexer.expect_save_document().returning(|_| Ok(()));

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage.expect_is_applied().returning(|_| Ok(false));
        cursor_storage.expect_mark_applied().returning(|_| Ok(()));
        cursor_storage.expect_save_cursor().returning(|_| Ok(()));

        ChangelogClient::new(
            mock_grpc,
            document_storage,
            Arc::new(indexer),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        )
    }

    #[tokio::test]
    async fn consume_applies_events_chained_through_previous_id() {
        let (first_id, second_id) = (Uuid::now_v7(), Uuid::now_v7());
        let first = proto_event(first_id, Uuid::now_v7());
        let second = ProtoChangelogEvent {
            previous_id: first_id.to_string(),
            ..proto_event(second_id, Uuid::now_v7())
        };
        // The persisted cursor is only read once per pass (and is mocked to
        // stay nil), so the second event is checked against the first one's
        // id, as this pass saw it.
        let client = client_streaming(vec![first, second]);

        let mut consumed = Vec::new();
        client
            .consume_once(&mut |event| consumed.push(event.id()))
            .await
            .unwrap();

        assert_eq!(consumed, vec![first_id, second_id]);
    }

    #[tokio::test]
    async fn consume_rejects_an_event_that_does_not_follow_the_last_one_before_decrypting_it() {
        let first_id = Uuid::now_v7();
        let first = proto_event(first_id, Uuid::now_v7());
        // Chained to an event this pass never saw, and not decryptable here
        // either: the gap is what gets reported.
        let gap = ProtoChangelogEvent {
            id: Uuid::now_v7().to_string(),
            encrypted_content: vec![0; 128],
            previous_id: Uuid::now_v7().to_string(),
        };
        let client = client_streaming(vec![first, gap]);

        let mut consumed = Vec::new();
        let err = client
            .consume_once(&mut |event| consumed.push(event.id()))
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidChangelogEvent);
        assert_eq!(consumed, vec![first_id]);
    }

    #[tokio::test]
    async fn consume_rejects_a_first_event_chained_to_another_one() {
        let event = ProtoChangelogEvent {
            previous_id: Uuid::now_v7().to_string(),
            ..proto_event(Uuid::now_v7(), Uuid::now_v7())
        };
        let client = client_streaming(vec![event]);

        let err = client.consume_once(&mut |_| {}).await.unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidChangelogEvent);
    }

    #[tokio::test(start_paused = true)]
    async fn run_consume_loop_never_returns_and_retries_after_an_error() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_in_mock = attempts.clone();
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            if attempts_in_mock.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(ErrorKind::InvalidChangelogEvent("boom".to_string()).into())
            } else {
                // A never-ending stream, mirroring the real `WatchEvents`
                // RPC staying open: this keeps the retried attempt parked
                // awaiting the next event, rather than looping back into
                // `consume_since` in a tight, unyielding busy loop the way
                // an immediately-exhausted stream would.
                Ok(futures::stream::pending().boxed())
            }
        });

        let document_storage = MockStorage::new();

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));

        let client = ChangelogClient::new(
            mock_grpc,
            document_storage,
            idle_indexer(),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        let mut callback = |_| {};
        let consume_fut = client.run_consume_loop(&mut callback);
        tokio::pin!(consume_fut);

        // The first attempt fails immediately; the loop waits before retrying.
        tokio::time::timeout(std::time::Duration::from_millis(0), &mut consume_fut)
            .await
            .unwrap_err();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        // Once the delay is over it retries, parking in the second attempt's
        // never-ending stream rather than returning.
        tokio::time::advance(CONSUME_RETRY_INITIAL_DELAY).await;
        tokio::time::timeout(std::time::Duration::from_millis(0), &mut consume_fut)
            .await
            .unwrap_err();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn run_consume_loop_waits_before_reopening_a_stream_that_ended_cleanly() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_in_mock = attempts.clone();
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            attempts_in_mock.fetch_add(1, Ordering::SeqCst);
            Ok(futures::stream::empty().boxed())
        });
        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        let client = ChangelogClient::new(
            mock_grpc,
            MockStorage::new(),
            idle_indexer(),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        let mut callback = |_| {};
        let consume_fut = client.run_consume_loop(&mut callback);
        tokio::pin!(consume_fut);

        tokio::time::timeout(std::time::Duration::from_millis(0), &mut consume_fut)
            .await
            .unwrap_err();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        // Not reopened before the delay is over...
        tokio::time::advance(CONSUME_RECONNECT_DELAY / 2).await;
        tokio::time::timeout(std::time::Duration::from_millis(0), &mut consume_fut)
            .await
            .unwrap_err();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        // ...and reopened once it is.
        tokio::time::advance(CONSUME_RECONNECT_DELAY / 2).await;
        tokio::time::timeout(std::time::Duration::from_millis(0), &mut consume_fut)
            .await
            .unwrap_err();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    /// A client whose every `consume_since` fails, counting the attempts in
    /// `attempts`.
    fn always_failing_client(
        attempts: Arc<std::sync::atomic::AtomicU32>,
    ) -> ChangelogClient<MockStorage, MockScriptStorage, MockSessionStorage, MockCursorStorage>
    {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(ErrorKind::InvalidChangelogEvent("boom".to_string()).into())
        });

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));

        ChangelogClient::new(
            mock_grpc,
            MockStorage::new(),
            idle_indexer(),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        )
    }

    #[tokio::test(start_paused = true)]
    async fn run_consume_loop_doubles_its_retry_delay_after_each_failure() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let attempts = Arc::new(AtomicU32::new(0));
        let client = always_failing_client(attempts.clone());

        let mut callback = |_| {};
        let consume_fut = client.run_consume_loop(&mut callback);
        tokio::pin!(consume_fut);

        tokio::time::timeout(std::time::Duration::from_millis(0), &mut consume_fut)
            .await
            .unwrap_err();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        // Second attempt after the initial delay...
        tokio::time::advance(CONSUME_RETRY_INITIAL_DELAY).await;
        tokio::time::timeout(std::time::Duration::from_millis(0), &mut consume_fut)
            .await
            .unwrap_err();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);

        // ...the third only after twice as long.
        tokio::time::advance(CONSUME_RETRY_INITIAL_DELAY).await;
        tokio::time::timeout(std::time::Duration::from_millis(0), &mut consume_fut)
            .await
            .unwrap_err();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);

        tokio::time::advance(CONSUME_RETRY_INITIAL_DELAY).await;
        tokio::time::timeout(std::time::Duration::from_millis(0), &mut consume_fut)
            .await
            .unwrap_err();
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn stop_consume_job_interrupts_a_loop_waiting_to_retry() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let attempts = Arc::new(AtomicU32::new(0));
        let client = Arc::new(always_failing_client(attempts.clone()));

        let loop_client = client.clone();
        let loop_task =
            tokio::spawn(async move { loop_client.run_consume_loop(&mut |_| {}).await });

        // Let the first attempt fail and the loop start waiting to retry.
        while attempts.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }

        client.stop_consume_job();

        tokio::time::timeout(std::time::Duration::from_millis(100), loop_task)
            .await
            .expect("run_consume_loop did not return promptly after stop_consume_job()")
            .unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stop_consume_job_interrupts_a_loop_parked_on_an_idle_stream() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_consume_since()
            .returning(|_| Ok(futures::stream::pending().boxed()));

        let document_storage = MockStorage::new();

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));

        let client = Arc::new(ChangelogClient::new(
            mock_grpc,
            document_storage,
            idle_indexer(),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        ));

        let loop_client = client.clone();
        let loop_task =
            tokio::spawn(async move { loop_client.run_consume_loop(&mut |_| {}).await });

        // Give the loop a chance to actually start and park on the
        // never-ending stream before stopping it.
        tokio::task::yield_now().await;

        client.stop_consume_job();

        tokio::time::timeout(std::time::Duration::from_secs(1), loop_task)
            .await
            .expect("run_consume_loop did not return promptly after stop_consume_job()")
            .unwrap();
    }

    #[tokio::test]
    async fn stop_consume_job_called_before_the_loop_starts_makes_it_return_immediately() {
        // No `expect_consume_since()`: the mock panics if it's called,
        // which is how this test proves the loop never opens a stream when
        // it's already been stopped.
        let mock_grpc = MockFydeClient::new();
        let document_storage = MockStorage::new();
        let cursor_storage = MockCursorStorage::new();

        let client = ChangelogClient::new(
            mock_grpc,
            document_storage,
            idle_indexer(),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        client.stop_consume_job();

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.run_consume_loop(&mut |_| {}),
        )
        .await
        .expect("run_consume_loop did not return promptly");
    }

    #[tokio::test]
    async fn start_consume_job_returns_as_soon_as_the_loop_is_spawned() {
        // The stream never yields anything and never closes, so if
        // `start_consume_job` awaited the loop inline instead of spawning
        // it onto its own task, this test would time out.
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_consume_since()
            .returning(|_| Ok(futures::stream::pending().boxed()));

        let document_storage = MockStorage::new();

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));

        let client = Arc::new(ChangelogClient::new(
            mock_grpc,
            document_storage,
            idle_indexer(),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        ));

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.clone().start_consume_job(Box::new(|_| {})),
        )
        .await
        .expect("start_consume_job did not return promptly")
        .unwrap();

        // Stop the background task the call above spawned, so it doesn't
        // keep running after the test ends.
        client.stop_consume_job();
    }

    #[tokio::test]
    async fn send_encrypts_the_event_before_it_reaches_the_transport_layer() {
        let document_id = Uuid::now_v7();
        let metadata = FakeMetadata::new().build();

        let mut mock_grpc = MockFydeClient::new();
        let expected_metadata = metadata.clone();
        mock_grpc
            .expect_record_event()
            .withf(move |encrypted_content| {
                let (event_type, id, content, decrypted_metadata) = futures::executor::block_on(
                    crypto::decrypt_event(&*fake_secrets(), encrypted_content),
                )
                .unwrap();
                event_type == EventType::SaveDocument
                    && id == document_id
                    && content == Some(b"body".to_vec())
                    && decrypted_metadata == Some(expected_metadata.clone())
            })
            .returning(|_| Ok(Uuid::now_v7()));

        let client = ChangelogClient::new(
            mock_grpc,
            MockStorage::new(),
            idle_indexer(),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            MockCursorStorage::new(),
            reachable_server_state(),
            fake_secrets(),
        );

        client
            .send(
                EventType::SaveDocument,
                document_id,
                Some(b"body"),
                Some(&metadata),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn send_refuses_an_event_larger_than_the_server_accepts() {
        // No `expect_record_event()`: the mock panics if it reaches the
        // transport layer.
        let client = ChangelogClient::new(
            MockFydeClient::new(),
            MockStorage::new(),
            idle_indexer(),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            MockCursorStorage::new(),
            reachable_server_state(),
            fake_secrets(),
        );

        let err = client
            .send(
                EventType::SaveDocument,
                Uuid::now_v7(),
                Some(&vec![0; super::super::MAX_EVENT_BYTES]),
                Some(&FakeMetadata::new().build()),
            )
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::DocumentTooLarge);
    }

    /// A client streaming `events` from the beginning (nil cursor) to
    /// `indexer`, with no document already cached, which must not apply
    /// (nor skip) any of them.
    fn client_applying_nothing(
        events: Vec<ProtoChangelogEvent>,
        indexer: MockIndexer,
    ) -> ChangelogClient<MockStorage, MockScriptStorage, MockSessionStorage, MockCursorStorage>
    {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(events.clone().into_iter().map(Ok)).boxed())
        });
        let mut document_storage = MockStorage::new();
        document_storage
            .expect_find_document_by_checksum()
            .returning(|_| Ok(None));

        // No `mark_applied`/`save_cursor`: the mock panics if the event is
        // recorded as applied or the cursor moves past it.
        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage.expect_is_applied().returning(|_| Ok(false));

        ChangelogClient::new(
            mock_grpc,
            document_storage,
            Arc::new(indexer),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        )
    }

    #[tokio::test]
    async fn consume_prepares_the_indexing_model_once_per_pass_before_the_first_document() {
        let (first_id, second_id) = (Uuid::now_v7(), Uuid::now_v7());
        let first = proto_event_with_type(first_id, Uuid::now_v7(), EventType::UpdateMetadata);
        let second = ProtoChangelogEvent {
            previous_id: first_id.to_string(),
            ..proto_event(second_id, Uuid::now_v7())
        };

        let mut sequence = mockall::Sequence::new();
        let mut indexer = MockIndexer::new();
        indexer
            .expect_prepare()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|| Ok(()));
        indexer
            .expect_update_metadata()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Ok(()));
        indexer
            .expect_save_document()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(()));

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(first.clone()), Ok(second.clone())]).boxed())
        });
        let mut document_storage = MockStorage::new();
        document_storage
            .expect_find_document_by_checksum()
            .returning(|_| Ok(None));
        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage.expect_is_applied().returning(|_| Ok(false));
        cursor_storage
            .expect_mark_applied()
            .times(2)
            .returning(|_| Ok(()));
        cursor_storage
            .expect_save_cursor()
            .times(2)
            .returning(|_| Ok(()));
        let client = ChangelogClient::new(
            mock_grpc,
            document_storage,
            Arc::new(indexer),
            MockScriptStorage::new(),
            MockSessionStorage::new(),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    #[tokio::test]
    async fn consume_applies_no_document_until_the_indexing_model_is_available() {
        let event_id = Uuid::now_v7();
        // No `save_document`: the mock panics if the document is cached
        // without its embeddings.
        let mut indexer = MockIndexer::new();
        indexer
            .expect_prepare()
            .times(1)
            .returning(|| Err(ErrorKind::ModelDownloadStatus(503).into()));
        let client = client_applying_nothing(vec![proto_event(event_id, Uuid::now_v7())], indexer);

        let mut called_back = false;
        let err = client
            .consume_once(&mut |_| called_back = true)
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::ModelDownloadFailed);
        assert!(err.to_string().contains(&event_id.to_string()), "{err}");
        assert!(!called_back);
    }

    #[tokio::test]
    async fn consume_leaves_the_cursor_before_a_document_that_fails_to_index() {
        let event_id = Uuid::now_v7();
        let mut indexer = ready_indexer();
        indexer
            .expect_save_document()
            .times(1)
            .returning(|_| Err(ErrorKind::InvalidModel("boom".to_string()).into()));
        let client = client_applying_nothing(vec![proto_event(event_id, Uuid::now_v7())], indexer);

        let mut called_back = false;
        let err = client
            .consume_once(&mut |_| called_back = true)
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::ModelLoadFailed);
        assert!(
            err.to_string()
                .contains(&format!("failed to apply changelog event {event_id}")),
            "{err}"
        );
        assert!(!called_back);
    }

    #[tokio::test]
    async fn consume_leaves_the_cursor_before_a_metadata_update_that_fails_to_index() {
        let mut indexer = ready_indexer();
        indexer
            .expect_update_metadata()
            .times(1)
            .returning(|_, _| Err(ErrorKind::InvalidModel("boom".to_string()).into()));
        let client = client_applying_nothing(
            vec![proto_event_with_type(
                Uuid::now_v7(),
                Uuid::now_v7(),
                EventType::UpdateMetadata,
            )],
            indexer,
        );

        let err = client.consume_once(&mut |_| {}).await.unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::ModelLoadFailed);
    }

    /// Runs `consume_once` on `client` `passes` times, each expected to
    /// fail, returning the error lines logged meanwhile.
    async fn failing_passes_logs(
        client: &ChangelogClient<
            MockStorage,
            MockScriptStorage,
            MockSessionStorage,
            MockCursorStorage,
        >,
        passes: u32,
    ) -> Vec<String> {
        use tracing_subscriber::prelude::*;

        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let layer = crate::tools::log::CallbackLayer {
            callback: Arc::new(move |level, line| {
                if level == crate::LogLevel::Error {
                    sink.lock().unwrap().push(line);
                }
            }),
        };
        let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(layer));
        for _ in 0..passes {
            client.consume_once(&mut |_| {}).await.unwrap_err();
        }

        lines.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn consume_reports_an_event_failing_pass_after_pass_as_blocking_sync() {
        let event_id = Uuid::now_v7();
        let mut indexer = ready_indexer();
        indexer
            .expect_save_document()
            .returning(|_| Err(ErrorKind::InvalidModel("boom".to_string()).into()));
        let client = client_applying_nothing(vec![proto_event(event_id, Uuid::now_v7())], indexer);

        let logs = failing_passes_logs(&client, POISON_PILL_PASSES - 1).await;
        assert_eq!(logs, Vec::<String>::new());

        let logs = failing_passes_logs(&client, 2).await;
        assert_eq!(logs.len(), 2, "{logs:?}");
        assert!(
            logs.iter()
                .all(|line| line.contains(&event_id.to_string()) && line.contains("blocking sync")),
            "{logs:?}"
        );
    }

    #[tokio::test]
    async fn consume_counts_failing_passes_afresh_for_another_event() {
        let (first_id, second_id) = (Uuid::now_v7(), Uuid::now_v7());
        let mut indexer = ready_indexer();
        indexer
            .expect_save_document()
            .returning(|_| Err(ErrorKind::InvalidModel("boom".to_string()).into()));
        let first = client_applying_nothing(vec![proto_event(first_id, Uuid::now_v7())], indexer);
        let logs = failing_passes_logs(&first, POISON_PILL_PASSES - 1).await;
        assert_eq!(logs, Vec::<String>::new());

        // The same client state, now failing on another event.
        let mut indexer = ready_indexer();
        indexer
            .expect_save_document()
            .returning(|_| Err(ErrorKind::InvalidModel("boom".to_string()).into()));
        let second = client_applying_nothing(vec![proto_event(second_id, Uuid::now_v7())], indexer);
        *second.failure_streak.lock().unwrap() = first.failure_streak.lock().unwrap().clone();
        let logs = failing_passes_logs(&second, 1).await;

        assert_eq!(logs, Vec::<String>::new());
        assert_eq!(
            *second.failure_streak.lock().unwrap(),
            Some((second_id.to_string(), 1))
        );
    }

    #[tokio::test]
    async fn consume_forgets_an_events_failing_passes_once_it_applies() {
        let event_id = Uuid::now_v7();
        let client = client_streaming(vec![proto_event(event_id, Uuid::now_v7())]);
        *client.failure_streak.lock().unwrap() =
            Some((event_id.to_string(), POISON_PILL_PASSES - 1));

        client.consume_once(&mut |_| {}).await.unwrap();

        assert_eq!(*client.failure_streak.lock().unwrap(), None);
    }

    #[tokio::test]
    async fn consume_does_not_count_an_unavailable_model_against_the_event() {
        let mut indexer = MockIndexer::new();
        indexer
            .expect_prepare()
            .returning(|| Err(ErrorKind::ModelDownloadStatus(503).into()));
        let client =
            client_applying_nothing(vec![proto_event(Uuid::now_v7(), Uuid::now_v7())], indexer);

        let logs = failing_passes_logs(&client, POISON_PILL_PASSES + 1).await;

        assert_eq!(logs, Vec::<String>::new());
        assert_eq!(*client.failure_streak.lock().unwrap(), None);
    }
}
