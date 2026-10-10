use std::fs::File;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::watch;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::domains::documents::{Document, Metadata, Storage as DocumentStorage};
use crate::domains::secrets::Service as SecretsService;
use crate::domains::server_state::Service as ServerStateService;
use crate::domains::sessions::SessionsClient;
use crate::{Error, ErrorContext as _, Result};

use super::Service;
use super::crypto;
use super::grpc_client::{ChangelogEvent as ProtoChangelogEvent, FydeClient, GrpcClient};
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

        let (event_type, document_id, content, metadata) =
            crypto::decrypt_event(secrets, &proto.encrypted_content)
                .await
                .with_context(|| format!("failed to decrypt changelog event {id}"))?;

        Ok(Self {
            id,
            event_type,
            document_id,
            content,
            metadata,
        })
    }
}

/// Returns the smallest id strictly greater than `id`, used to advance the
/// consumption cursor past an already-processed entry: ids are unique and
/// compared byte-for-byte, so the 128-bit successor of `id` is guaranteed
/// to be strictly greater than it and to skip no real id in between — it
/// doesn't need to be a valid UUIDv7 itself, it's only ever used as a query
/// bound (mirrors `DefaultService::consume_since` on the server).
fn successor(id: Uuid) -> Uuid {
    Uuid::from_u128(id.as_u128().wrapping_add(1))
}

/// Path to the advisory lock file arbitrating write access to the local
/// document cache across processes consuming the same changelog: only the
/// process holding this lock is allowed to persist documents/metadata via
/// [`ChangelogClient::consume_once`] (see [`ChangelogClient::write_lock`]'s
/// doc comment). Lives in the OS default temporary directory, shared by
/// every process on the machine, rather than under the per-account SQLite
/// database, since the lock's purpose is to arbitrate across processes, not
/// accounts.
fn write_lock_path() -> std::path::PathBuf {
    std::env::temp_dir().join("fyde-changelog-consume.lock")
}

/// A client for the fyde server's changelog service. Generic over the
/// [`DocumentStorage`] implementation used by [`Service::consume`] to cache
/// documents materialized from consumed events, and the [`CursorStorage`]
/// implementation used to track how far the changelog has been consumed.
pub(super) struct ChangelogClient<D: DocumentStorage, O: CursorStorage> {
    grpc: Box<dyn FydeClient>,
    document_storage: D,
    cursor_storage: O,
    server_state: Arc<dyn ServerStateService>,
    secrets: Arc<dyn SecretsService>,
    /// Signals [`Service::stop_consume_job`] to the task spawned by
    /// [`Service::start_consume_job`]. A `watch` channel (rather than
    /// [`tokio::sync::Notify`]) so the signal is level-triggered: a
    /// `stop_consume_job()` call is never lost even if it lands before the
    /// task starts watching it or between two of its passes.
    stop: watch::Sender<bool>,
    /// Path to this client's advisory write lock file — [`write_lock_path`]
    /// for a real, `pub(super)::new`-constructed client, or a unique
    /// per-instance path under test (see [`Self::with_grpc`]) so parallel
    /// tests each arbitrate their own lock rather than contending over one
    /// shared file.
    lock_path: std::path::PathBuf,
    /// Holds the open, OS-locked handle to [`Self::lock_path`] for as long
    /// as this process has exclusive write permission over the local
    /// document cache, acquired by [`Service::start_consume_job`] and
    /// released by [`Service::stop_consume_job`]. `None` whenever another
    /// process already holds it — in which case [`Self::consume_once`]
    /// still consumes every event and still invokes the caller's callback,
    /// but skips persisting the document/metadata locally, since only the
    /// lock holder is allowed to write. The OS releases the underlying
    /// `flock` automatically if the process exits without calling
    /// [`Service::stop_consume_job`] (e.g. a crash), so the lock can never
    /// outlive the process holding it.
    write_lock: Mutex<Option<File>>,
}

/// Delay between reachability checks in [`ChangelogClient::consume_once`]
/// while waiting for the server to come back up.
const SERVER_REACHABILITY_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(200);

impl<D: DocumentStorage, O: CursorStorage> ChangelogClient<D, O> {
    /// Creates a client for the changelog service using the shared
    /// `channel` connection to the fyde server, using `document_storage` to
    /// cache documents materialized by [`Service::consume`],
    /// `cursor_storage` to track its progress through the changelog,
    /// `server_state` to wait for the server to be reachable before opening
    /// a changelog stream, and `secrets` to read the account master key
    /// [`crypto`] derives every event's KEK from.
    pub(super) async fn new(
        channel: Channel,
        document_storage: D,
        cursor_storage: O,
        sessions: Arc<SessionsClient>,
        server_state: Arc<dyn ServerStateService>,
        secrets: Arc<dyn SecretsService>,
    ) -> Result<Self> {
        Ok(Self {
            grpc: Box::new(GrpcClient::new(channel, sessions)),
            document_storage,
            cursor_storage,
            server_state,
            secrets,
            stop: watch::Sender::new(false),
            lock_path: write_lock_path(),
            write_lock: Mutex::new(None),
        })
    }

    /// Creates a client from an already-constructed [`FydeClient`], for
    /// testing against a [`super::grpc_client::MockFydeClient`] instead of a
    /// live server.
    #[cfg(test)]
    fn with_grpc(
        grpc: impl FydeClient + 'static,
        document_storage: D,
        cursor_storage: O,
        server_state: Arc<dyn ServerStateService>,
        secrets: Arc<dyn SecretsService>,
    ) -> Self {
        Self {
            grpc: Box::new(grpc),
            document_storage,
            cursor_storage,
            server_state,
            secrets,
            stop: watch::Sender::new(false),
            lock_path: std::env::temp_dir()
                .join(format!("fyde-changelog-consume-{}.lock", Uuid::new_v4())),
            write_lock: Mutex::new(None),
        }
    }

    /// Attempts to take exclusive ownership of [`Self::lock_path`], storing
    /// the locked handle in `self.write_lock` on success. Never fails the
    /// caller: if the file can't be opened or is already locked by another
    /// process, this just leaves `self.write_lock` as `None` and logs why,
    /// so [`Self::consume_once`] falls back to read-only consumption
    /// instead of erroring out of [`Service::start_consume_job`] entirely.
    fn acquire_write_lock(&self) {
        let path = &self.lock_path;

        let file = match File::create(path) {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!(
                    "failed to open changelog write lock file {path:?}, \
                     falling back to read-only consumption: {err:?}"
                );
                return;
            }
        };

        match file.try_lock() {
            Ok(()) => {
                *self.write_lock.lock().unwrap() = Some(file);
            }
            Err(err) => {
                tracing::warn!(
                    "changelog write lock {path:?} already held by another process, \
                     falling back to read-only consumption: {err:?}"
                );
            }
        }
    }

    /// Releases the write lock taken by [`Self::acquire_write_lock`], if
    /// this process is holding it. Idempotent, and safe to call whether or
    /// not the lock was ever successfully acquired.
    fn release_write_lock(&self) {
        self.write_lock.lock().unwrap().take();
    }

    /// Whether this process currently holds [`Self::lock_path`], and is
    /// therefore allowed to persist documents/metadata locally in
    /// [`Self::consume_once`].
    fn has_write_permission(&self) -> bool {
        self.write_lock.lock().unwrap().is_some()
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

        while let Some(proto_event) = stream.next().await {
            let proto_event = proto_event.context("failed to read next changelog event")?;
            let event = ChangelogEvent::from_proto(proto_event, &*self.secrets).await?;

            match event.event_type {
                EventType::Created => {
                    let (Some(content), Some(metadata)) = (&event.content, &event.metadata) else {
                        return Err(Error::InvalidChangelogEvent(format!(
                            "created event {} for document {} is missing content or metadata",
                            event.id, event.document_id
                        )));
                    };

                    if self.has_write_permission() {
                        let document =
                            Document::new(event.document_id, content.clone(), metadata.clone());
                        self.document_storage
                            .save_document(&document)
                            .await
                            .with_context(|| {
                                format!("failed to cache document {} locally", event.document_id)
                            })?;
                    }
                }
                EventType::UpdateMetadata => {
                    let Some(metadata) = &event.metadata else {
                        return Err(Error::InvalidChangelogEvent(format!(
                            "update metadata event {} for document {} is missing metadata",
                            event.id, event.document_id
                        )));
                    };

                    if self.has_write_permission() {
                        self.document_storage
                            .update_metadata(event.document_id, metadata)
                            .await
                            .with_context(|| {
                                format!(
                                    "failed to update cached document {} locally",
                                    event.document_id
                                )
                            })?;
                    }
                }
                EventType::Deleted => {}
            }

            let next_cursor = successor(event.id);
            self.cursor_storage
                .save_cursor(next_cursor)
                .await
                .with_context(|| format!("failed to persist changelog cursor {next_cursor}"))?;

            callback(event);
        }

        Ok(())
    }

    /// The retry loop spawned by [`Service::start_consume_job`] onto its own
    /// background task: repeats [`Self::consume_once`] from the persisted
    /// cursor, logging and immediately retrying on either an error or a
    /// clean end of stream, until [`Service::stop_consume_job`] signals it
    /// to return. Split out as its own inherent method (rather than inlined
    /// in `start_consume_job`'s spawned task) so tests can exercise the loop
    /// directly, synchronously, without needing to observe a detached task's
    /// completion.
    async fn run_consume_loop(&self, callback: &mut (dyn FnMut(ChangelogEvent) + Send)) {
        let mut stop = self.stop.subscribe();

        while !*stop.borrow() {
            tokio::select! {
                result = self.consume_once(callback) => {
                    if let Err(err) = result {
                        tracing::error!("changelog consume failed, retrying: {err:?}");
                    }
                }
                _ = stop.changed() => {}
            }
        }
    }
}

#[async_trait]
impl<D: DocumentStorage + 'static, O: CursorStorage + 'static> Service for ChangelogClient<D, O> {
    async fn ensure_master_key(&self) -> Result<()> {
        crypto::ensure_master_key(&*self.secrets).await
    }

    async fn send(
        &self,
        event_type: EventType,
        document_id: Uuid,
        content: Option<&[u8]>,
        metadata: Option<&Metadata>,
    ) -> Result<()> {
        let encrypted_content =
            crypto::encrypt_event(&*self.secrets, event_type, document_id, content, metadata)
                .await
                .context("failed to encrypt changelog event")?;

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

        // Re-acquired on every start, since a prior `stop_consume_job` call
        // released it (see `write_lock`'s doc comment).
        self.acquire_write_lock();

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
        self.release_write_lock();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::super::grpc_client::MockFydeClient;
    use super::super::storage::MockCursorStorage;
    use super::*;
    use crate::domains::documents::{FakeMetadata, MockStorage};
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
        let encoded = crate::domains::users::encode_master_key(b"the-account-master-key").unwrap();
        mock.expect_get()
            .withf(|key| key == crate::domains::secrets::MASTER_KEY_SECRET)
            .returning(move |_| Ok(Some(encoded.clone())));
        Arc::new(mock)
    }

    fn proto_event(id: Uuid, document_id: Uuid) -> ProtoChangelogEvent {
        proto_event_with_type(id, document_id, EventType::Created)
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
            .expect_save_document()
            .withf(move |document| document.id() == document_id && document.content() == b"content")
            .times(1)
            .returning(|_| Ok(()));

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == successor(event_id))
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );
        client.acquire_write_lock();

        let received = std::sync::Arc::new(Mutex::new(Vec::new()));
        let received_in_callback = received.clone();
        let mut callback = move |event| received_in_callback.lock().unwrap().push(event);
        client.consume_once(&mut callback).await.unwrap();

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].id, event_id);
        assert_eq!(received[0].document_id, document_id);
        assert_eq!(received[0].event_type, EventType::Created);
    }

    #[tokio::test]
    async fn consume_still_invokes_the_callback_but_skips_saving_without_the_write_lock() {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(proto_event(event_id, document_id))]).boxed())
        });

        // No `expect_save_document()` set up: the mock panics if it's
        // called, which is how this test proves the document is never
        // persisted locally when this client doesn't hold the write lock.
        let document_storage = MockStorage::new();

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == successor(event_id))
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );
        // No `acquire_write_lock()` call: this client never holds the lock.

        let received = std::sync::Arc::new(Mutex::new(Vec::new()));
        let received_in_callback = received.clone();
        let mut callback = move |event| received_in_callback.lock().unwrap().push(event);
        client.consume_once(&mut callback).await.unwrap();

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].document_id, document_id);
    }

    #[tokio::test]
    async fn a_second_client_holding_the_same_lock_path_fails_to_acquire_it() {
        let mock_grpc = MockFydeClient::new();
        let document_storage = MockStorage::new();
        let cursor_storage = MockCursorStorage::new();

        let first = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );
        first.acquire_write_lock();
        assert!(first.has_write_permission());

        // Build a second client pointed at the same lock file as `first`,
        // simulating a second process/client instance racing for it.
        let mut second = ChangelogClient::with_grpc(
            MockFydeClient::new(),
            MockStorage::new(),
            MockCursorStorage::new(),
            reachable_server_state(),
            fake_secrets(),
        );
        second.lock_path = first.lock_path.clone();

        second.acquire_write_lock();
        assert!(!second.has_write_permission());

        first.release_write_lock();
        second.acquire_write_lock();
        assert!(second.has_write_permission());
    }

    #[tokio::test]
    async fn start_consume_job_acquires_the_write_lock_and_stop_consume_job_releases_it() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_consume_since()
            .returning(|_| Ok(futures::stream::pending().boxed()));

        let document_storage = MockStorage::new();

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));

        let client = Arc::new(ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        ));

        client
            .clone()
            .start_consume_job(Box::new(|_| {}))
            .await
            .unwrap();
        assert!(client.has_write_permission());

        client.stop_consume_job();
        assert!(!client.has_write_permission());
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
            .expect_save_document()
            .returning(|_| Ok(()));

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
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

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
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
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == successor(event_id))
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
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

        let mut document_storage = MockStorage::new();
        document_storage
            .expect_update_metadata()
            .withf(move |id, _metadata| *id == document_id)
            .times(1)
            .returning(|_, _| Ok(()));

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == successor(event_id))
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );
        client.acquire_write_lock();

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    #[tokio::test]
    async fn consume_errors_on_a_created_event_missing_content_or_metadata() {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let encrypted_content = futures::executor::block_on(crypto::encrypt_event(
            &*fake_secrets(),
            EventType::Created,
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
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        let result = client.consume_once(&mut |_| {}).await;

        assert!(matches!(result, Err(Error::InvalidChangelogEvent(_))));
    }

    #[tokio::test]
    async fn consume_resumes_from_the_persisted_cursor() {
        let document_id = Uuid::now_v7();
        let cursor = Uuid::now_v7();
        let event_id = successor(cursor);

        let mut mock_grpc = MockFydeClient::new();
        mock_grpc
            .expect_consume_since()
            .withf(move |id| *id == Some(cursor))
            .returning(move |_| {
                Ok(futures::stream::iter(vec![Ok(proto_event(event_id, document_id))]).boxed())
            });

        let mut document_storage = MockStorage::new();
        document_storage
            .expect_save_document()
            .returning(|_| Ok(()));

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(move || Ok(cursor));
        cursor_storage
            .expect_save_cursor()
            .withf(move |id| *id == successor(event_id))
            .times(1)
            .returning(|_| Ok(()));

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    #[tokio::test]
    async fn run_consume_loop_never_returns_and_retries_after_an_error() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_in_mock = attempts.clone();
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_consume_since().returning(move |_| {
            if attempts_in_mock.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(Error::InvalidChangelogEvent("boom".to_string()))
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

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        let mut callback = |_| {};
        let consume_fut = client.run_consume_loop(&mut callback);
        tokio::pin!(consume_fut);

        // The first attempt fails immediately, and with no retry delay the
        // loop should retry right away — parking in the second attempt's
        // never-ending stream rather than returning.
        tokio::time::timeout(std::time::Duration::from_millis(0), &mut consume_fut)
            .await
            .unwrap_err();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
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

        let client = Arc::new(ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
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

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
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

        let client = Arc::new(ChangelogClient::with_grpc(
            mock_grpc,
            document_storage,
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
                event_type == EventType::Created
                    && id == document_id
                    && content == Some(b"body".to_vec())
                    && decrypted_metadata == Some(expected_metadata.clone())
            })
            .returning(|_| Ok(Uuid::now_v7()));

        let client = ChangelogClient::with_grpc(
            mock_grpc,
            MockStorage::new(),
            MockCursorStorage::new(),
            reachable_server_state(),
            fake_secrets(),
        );

        client
            .send(
                EventType::Created,
                document_id,
                Some(b"body"),
                Some(&metadata),
            )
            .await
            .unwrap();
    }
}
