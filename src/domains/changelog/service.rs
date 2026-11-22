use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use futures::StreamExt;
use sha2::{Digest as _, Sha256};
use tokio::sync::watch;
use uuid::Uuid;

use crate::domains::documents::{Document, Indexer, Storage as DocumentStorage};
use crate::domains::folders::{Applier as FolderApplier, Folder, FolderPatch};
use crate::domains::scrapers::StateStorage as ScraperStateStorage;
use crate::domains::scripts::{InstalledScript, Storage as ScriptStorage};
use crate::domains::secrets::Service as SecretsService;
use crate::domains::server_state::Service as ServerStateService;
use crate::{Error, ErrorContext as _, ErrorKind, Result};

use super::Service;
use super::crypto;
use super::grpc_client::{ChangelogEvent as ProtoChangelogEvent, FydeClient};
use super::models::{Change, ChangelogEvent, SyncProgress};
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

        let (subject_id, change) = crypto::decrypt_event(secrets, &proto.encrypted_content)
            .await
            .with_context(|| format!("failed to decrypt changelog event {id}"))?;

        Ok(Self {
            id,
            subject_id,
            change,
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
/// [`DocumentStorage`] implementation used by the consume job to spot
/// documents already cached, the [`ScriptStorage`]
/// implementation it saves and deletes script installations in, the
/// [`ScraperStateStorage`] it clears an installation's scraper session in
/// when a new version of its script is installed (and all its scraper
/// state when it's uninstalled), and the [`CursorStorage`]
/// implementation used to track how far the changelog has been consumed.
pub(super) struct ChangelogClient<
    D: DocumentStorage,
    S: ScriptStorage,
    P: ScraperStateStorage,
    O: CursorStorage,
> {
    grpc: Box<dyn FydeClient>,
    document_storage: D,
    indexer: Arc<dyn Indexer>,
    script_storage: S,
    scraper_state_storage: P,
    folders: Arc<dyn FolderApplier>,
    cursor_storage: O,
    server_state: Arc<dyn ServerStateService>,
    secrets: Arc<dyn SecretsService>,
    /// The generation of the job allowed to run: [`Service::start_consume_job`]
    /// and [`Service::stop_consume_job`] each move it on, and a task spawned
    /// by `start_consume_job` runs only while it's still the one it was
    /// started as. A generation rather than a stop flag, so a job stopped
    /// then started again before the stopped task has seen the stop can't
    /// read the new start as its own and keep consuming beside the new one.
    /// A `watch` channel (rather than [`tokio::sync::Notify`]) so the signal
    /// is level-triggered: a stop is never lost even if it lands before the
    /// task starts watching it or between two of its passes.
    job: watch::Sender<u64>,
    /// The event the last passes failed on (by its id as the server sent
    /// it), and how many passes in a row did.
    failure_streak: Mutex<Option<(String, u32)>>,
    /// See [`Service::sync_progress`].
    progress: Mutex<SyncProgress>,
}

impl<D: DocumentStorage, S: ScriptStorage, P: ScraperStateStorage, O: CursorStorage>
    ChangelogClient<D, S, P, O>
{
    /// Creates a client for the changelog service over `grpc`, using
    /// `document_storage` to spot documents the consume job already
    /// cached, `indexer` to cache (and index) the ones it materializes,
    /// `script_storage` to save and delete the script installations it
    /// materializes, `scraper_state_storage` to clear the saved session
    /// of an installation whose script changes version (and everything kept
    /// under an uninstalled one), `folders` to apply folder
    /// events, `cursor_storage` to track its progress through the
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
        scraper_state_storage: P,
        folders: Arc<dyn FolderApplier>,
        cursor_storage: O,
        server_state: Arc<dyn ServerStateService>,
        secrets: Arc<dyn SecretsService>,
    ) -> Self {
        Self {
            grpc: Box::new(grpc),
            document_storage,
            indexer,
            script_storage,
            scraper_state_storage,
            folders,
            cursor_storage,
            server_state,
            secrets,
            job: watch::Sender::new(0),
            failure_streak: Mutex::new(None),
            progress: Mutex::new(SyncProgress::default()),
        }
    }

    fn progress(&self) -> std::sync::MutexGuard<'_, SyncProgress> {
        self.progress.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Asks the server how many events follow `since`, setting the job's
    /// total to those plus the ones it already consumed. A failure (an
    /// older server without `CountSince`, say) is only logged: the total is
    /// for display, and the pass goes on without it.
    async fn count_remaining(&self, since: Option<Uuid>) {
        match self.grpc.count_since(since).await {
            Ok(remaining) => {
                let mut progress = self.progress();
                progress.total = Some(progress.consumed + remaining);
            }
            Err(err) => tracing::warn!(
                "failed to count the changelog events left to consume, \
                 sync progress has no total: {err:?}"
            ),
        }
    }

    /// Counts one more event gone through by the job, raising the total if
    /// it was recorded after the server counted.
    fn event_consumed(&self) {
        let mut progress = self.progress();
        progress.consumed += 1;
        let consumed = progress.consumed;
        if let Some(total) = progress.total.as_mut() {
            *total = (*total).max(consumed);
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
        check_follows(&proto, offset)
            .with_context(|| format!("changelog event {:?} is out of sequence", proto.id))?;
        let digest: [u8; 32] = Sha256::digest(&proto.encrypted_content).into();
        let event = ChangelogEvent::from_proto(proto, &*self.secrets)
            .await
            .context("failed to decode changelog event")?;

        Ok((event, digest))
    }

    /// Applies `event` to local storage: caches (and indexes) the document
    /// it uploads, patches the document, folder or installation it changes
    /// (ignoring one that isn't saved here: a skipped duplicate, or one
    /// deleted meanwhile), removes the one it deletes.
    async fn apply(&self, event: &ChangelogEvent) -> Result<()> {
        let id = event.subject_id;
        let applied = || format!("failed to apply changelog event {}", event.id);
        match &event.change {
            Change::DocumentUploaded { content, metadata } => {
                // `upload` only rejects a duplicate already cached
                // here, so two uploads of the same content that
                // haven't synced yet (or from two devices) both get
                // published: keep the first one cached, skip the
                // rest.
                let duplicate_of = if metadata.checksum.is_empty() {
                    None
                } else {
                    self.document_storage
                        .find_document_by_checksum(&metadata.checksum)
                        .await
                        .with_context(|| {
                            format!(
                                "failed to look up local documents with the same content as document {id}"
                            )
                        })?
                };

                if let Some(existing) = duplicate_of {
                    tracing::warn!(
                        event_id = %event.id,
                        document_id = %id,
                        existing_document_id = %existing,
                        "skipping caching document {id}: a document with the same content is already cached locally as {existing}",
                    );
                    self.indexer.skip_document(id);
                    return Ok(());
                }

                let document = Document::new(id, content.clone(), metadata.clone());
                self.indexer
                    .save_document(&document)
                    .await
                    .with_context(applied)
            }
            Change::DocumentRenamed { .. }
            | Change::DocumentAttributesUpdated { .. }
            | Change::DocumentMoved { .. } => {
                let Some(mut metadata) = self
                    .document_storage
                    .get_metadata(id)
                    .await
                    .with_context(|| format!("failed to load document {id}'s metadata"))?
                else {
                    return Ok(());
                };
                match event.change.clone() {
                    Change::DocumentRenamed { name } => metadata.name = name,
                    Change::DocumentAttributesUpdated { r#type, attributes } => {
                        metadata.r#type = r#type;
                        metadata.attributes = attributes;
                    }
                    Change::DocumentMoved { folder_id } => metadata.folder_id = folder_id,
                    _ => unreachable!("matched above"),
                }
                self.indexer
                    .update_metadata(id, &metadata)
                    .await
                    .with_context(applied)
            }
            Change::DocumentDeleted => self.indexer.delete_document(id).await.with_context(applied),
            Change::ScriptInstalled {
                label,
                script,
                parameters,
                folder_id,
            } => {
                let installed = InstalledScript::new(
                    id,
                    label.clone(),
                    script.clone(),
                    parameters.clone(),
                    *folder_id,
                );
                let previous = self.installation(id).await?;
                self.reset_scraper_session_on_new_version(previous.as_ref(), &installed)
                    .await
                    .with_context(applied)?;
                self.save_installation(&installed).await
            }
            Change::InstallationCredentialsUpdated { .. }
            | Change::InstallationRenamed { .. }
            | Change::InstallationFolderChanged { .. }
            | Change::InstallationUpgraded { .. } => {
                let Some(mut installed) = self.installation(id).await? else {
                    return Ok(());
                };
                match event.change.clone() {
                    Change::InstallationCredentialsUpdated { parameters } => {
                        installed.set_parameters(parameters);
                    }
                    Change::InstallationRenamed { label } => installed.set_label(label),
                    Change::InstallationFolderChanged { folder_id } => {
                        installed.set_folder_id(folder_id);
                    }
                    Change::InstallationUpgraded { script } => {
                        let previous = installed.clone();
                        installed.set_script(script);
                        self.reset_scraper_session_on_new_version(Some(&previous), &installed)
                            .await
                            .with_context(applied)?;
                    }
                    _ => unreachable!("matched above"),
                }
                self.save_installation(&installed).await
            }
            Change::ScriptUninstalled => {
                self.script_storage
                    .delete_installed_script(id)
                    .await
                    .with_context(|| format!("failed to delete installation {id} locally"))?;
                self.scraper_state_storage
                    .delete_all(&id.to_string())
                    .await
                    .with_context(|| {
                        format!("failed to forget the scraper state of installation {id}")
                    })
            }
            Change::FolderCreated {
                parent_id,
                name,
                appearance,
            } => self
                .folders
                .apply_save(Folder {
                    id,
                    parent_id: *parent_id,
                    name: name.clone(),
                    appearance: *appearance,
                })
                .await
                .with_context(applied),
            Change::FolderRenamed { name } => self
                .folders
                .apply_patch(id, FolderPatch::Rename(name.clone()))
                .await
                .with_context(applied),
            Change::FolderAppearanceChanged { appearance } => self
                .folders
                .apply_patch(id, FolderPatch::Appearance(*appearance))
                .await
                .with_context(applied),
            Change::FolderMoved {
                parent_id,
                appearance,
            } => self
                .folders
                .apply_patch(
                    id,
                    FolderPatch::Move {
                        parent_id: *parent_id,
                        appearance: *appearance,
                    },
                )
                .await
                .with_context(applied),
            Change::FolderDeleted { contents } => self
                .folders
                .apply_delete(id, *contents)
                .await
                .with_context(applied),
        }
    }

    /// The installation `id` saved here, if any.
    async fn installation(&self, id: Uuid) -> Result<Option<InstalledScript>> {
        self.script_storage
            .find_installed_script(id)
            .await
            .with_context(|| format!("failed to load installation {id}"))
    }

    async fn save_installation(&self, installed: &InstalledScript) -> Result<()> {
        self.script_storage
            .save_installed_script(installed)
            .await
            .with_context(|| {
                format!(
                    "failed to save installation {} locally",
                    installed.installation_id()
                )
            })
    }

    /// Forgets the scraper session saved under `installed`'s installation id
    /// (the name [`crate::domains::scrapers::Service::run_installed`] keeps
    /// it under) unless the installation it replaces has the same script
    /// version: a new version must not trust what an older one recorded
    /// there, such as the URLs `fyde.helpers.save_pdf` skips as already
    /// saved, so its next run fetches every document again. Reinstalling the
    /// same version (e.g. with new credentials) keeps the session.
    async fn reset_scraper_session_on_new_version(
        &self,
        previous: Option<&InstalledScript>,
        installed: &InstalledScript,
    ) -> Result<()> {
        let installation_id = installed.installation_id();
        let version = installed.script().version();
        if previous.is_some_and(|previous| previous.script().version() == version) {
            return Ok(());
        }

        self.scraper_state_storage
            .delete_session(&installation_id.to_string())
            .await
            .with_context(|| {
                format!(
                    "failed to clear the scraper session of installation {installation_id} \
                     for its script version {version}"
                )
            })
    }

    /// One pass of the consume loop spawned by [`Service::start_consume_job`]:
    /// opens a stream from the persisted
    /// cursor and processes events until the stream ends or an error
    /// occurs. Split out of [`Self::run_consume_loop`], which retries it, so
    /// tests can exercise a single pass directly.
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

        self.count_remaining(since).await;

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
                self.event_consumed();
                offset = Some(event.id);
                continue;
            }

            // A model that can't be fetched isn't the event's fault, so isn't
            // counted against it.
            if !indexing_ready && event.event_type().writes_document() {
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
            self.event_consumed();
            callback(event);
        }

        Ok(())
    }

    /// Moves [`Self::job`] on to a new generation and returns it.
    fn next_job(&self) -> u64 {
        self.job.send_modify(|generation| *generation += 1);
        *self.job.borrow()
    }

    /// The retry loop spawned by [`Service::start_consume_job`] onto its own
    /// background task as job `generation`: repeats [`Self::consume_once`]
    /// from the persisted cursor — after [`CONSUME_RECONNECT_DELAY`] after a
    /// clean end of stream, after a growing delay (see
    /// [`CONSUME_RETRY_INITIAL_DELAY`]) after an error, which it logs — until
    /// [`Self::job`] moves past `generation` ([`Service::stop_consume_job`],
    /// or another `start_consume_job`), even mid-delay. Split out as its own inherent method (rather than inlined
    /// in `start_consume_job`'s spawned task) so tests can exercise the loop
    /// directly, synchronously, without needing to observe a detached task's
    /// completion.
    async fn run_consume_loop(
        &self,
        generation: u64,
        callback: &mut (dyn FnMut(ChangelogEvent) + Send),
    ) {
        let mut stop = self.job.subscribe();
        let mut retry_delay = CONSUME_RETRY_INITIAL_DELAY;

        while *stop.borrow_and_update() == generation {
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
    P: ScraperStateStorage + 'static,
    O: CursorStorage + 'static,
> Service for ChangelogClient<D, S, P, O>
{
    async fn ensure_master_key(&self) -> Result<()> {
        crypto::ensure_master_key(&*self.secrets).await
    }

    async fn send(&self, subject_id: Uuid, change: &Change) -> Result<()> {
        let event_type = change.event_type();
        let encrypted_content = crypto::encrypt_event(&*self.secrets, subject_id, change)
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
        // A new generation, which also ends any job still running: only
        // one task ever consumes.
        let generation = self.next_job();
        *self.progress() = SyncProgress::default();

        tokio::spawn(async move { self.run_consume_loop(generation, &mut *callback).await });

        Ok(())
    }

    fn stop_consume_job(&self) {
        // `send_modify`, not `send`: `send` is a no-op (doesn't even update
        // the value) when there are currently no receivers, which is
        // exactly the case where `stop_consume_job` is called before
        // `start_consume_job`'s task has subscribed — `send_modify` updates
        // the value unconditionally, so a task that starts afterwards still
        // sees it immediately via `subscribe`.
        self.next_job();
    }

    fn sync_progress(&self) -> SyncProgress {
        *self.progress()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::super::grpc_client::MockFydeClient;
    use super::super::models::EventType;
    use super::super::storage::MockCursorStorage;
    use super::*;
    use crate::domains::documents::{
        DocumentIndexer, FakeCachedEmbeddings, FakeMetadata, MockEmbedder, MockEmbeddingCache,
        MockIndexer, MockStorage,
    };
    use std::collections::{BTreeMap, HashMap};

    use crate::domains::documents::{Attribute, DocumentType, Metadata};
    use crate::domains::folders::{
        FakeFolder, FolderAppearance, FolderColor, FolderContents, FolderIcon, MockApplier,
    };
    use crate::domains::scrapers::MockStateStorage;
    use crate::domains::scripts::{
        FakeInstalledScript, FakeScript, MockStorage as MockScriptStorage,
    };
    use crate::domains::secrets::MockService as MockSecretsService;
    use crate::domains::server_state::MockService as MockServerState;

    /// A [`MockFydeClient`] whose server counts nothing left to consume, for
    /// tests where sync progress isn't under test.
    fn counting_grpc() -> MockFydeClient {
        let mut mock = MockFydeClient::new();
        mock.expect_count_since().returning(|_| Ok(0));
        mock
    }

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

    /// An upload of a document `document_id` whose content is `content`.
    fn proto_event(id: Uuid, document_id: Uuid) -> ProtoChangelogEvent {
        change_proto_event(
            id,
            document_id,
            &Change::DocumentUploaded {
                content: b"content".to_vec(),
                metadata: FakeMetadata::new().build(),
            },
        )
    }

    /// A proto event carrying `change` to `subject_id`, encrypted.
    fn change_proto_event(id: Uuid, subject_id: Uuid, change: &Change) -> ProtoChangelogEvent {
        let encrypted_content = futures::executor::block_on(crypto::encrypt_event(
            &*fake_secrets(),
            subject_id,
            change,
        ))
        .unwrap();
        ProtoChangelogEvent {
            id: id.to_string(),
            encrypted_content,
            previous_id: String::new(),
        }
    }

    /// The `DocumentRenamed` change used by tests not about its content.
    fn renaming() -> Change {
        Change::DocumentRenamed {
            name: "renamed.pdf".to_string(),
        }
    }

    #[tokio::test]
    async fn consume_decrypts_events_and_caches_the_document() {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let mut mock_grpc = counting_grpc();
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
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
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
        assert_eq!(received[0].event_type(), EventType::DocumentUploaded);
    }

    #[tokio::test]
    async fn consume_records_each_applied_event_by_the_digest_of_its_ciphertext() {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();
        let proto = proto_event(event_id, document_id);
        let expected: [u8; 32] = Sha256::digest(&proto.encrypted_content).into();

        let mut mock_grpc = counting_grpc();
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
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
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
        let proto = change_proto_event(
            event_id,
            document_id,
            &Change::DocumentUploaded {
                content: b"content".to_vec(),
                metadata: metadata.clone(),
            },
        );

        let mut mock_grpc = counting_grpc();
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
                Arc::new(tokio::sync::Notify::new()),
            )),
            MockScriptStorage::new(),
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
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

        let mut mock_grpc = counting_grpc();
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
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
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

        let mut mock_grpc = counting_grpc();
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
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
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

        let mut mock_grpc = counting_grpc();
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
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
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

    /// A gRPC mock streaming the single `event`.
    fn grpc_streaming(event: ProtoChangelogEvent) -> MockFydeClient {
        let mut mock_grpc = counting_grpc();
        mock_grpc
            .expect_consume_since()
            .returning(move |_| Ok(futures::stream::iter(vec![Ok(event.clone())]).boxed()));
        mock_grpc
    }

    /// A changelog client consuming the single `event` through
    /// `script_storage` and `state_storage`, and nothing else.
    fn client_applying_scripts(
        event: ProtoChangelogEvent,
        cursor_storage: MockCursorStorage,
        script_storage: MockScriptStorage,
        state_storage: MockStateStorage,
    ) -> ChangelogClient<MockStorage, MockScriptStorage, MockStateStorage, MockCursorStorage> {
        // No `MockStorage` expectations: a script event must never touch
        // the documents cache.
        ChangelogClient::new(
            grpc_streaming(event),
            MockStorage::new(),
            idle_indexer(),
            script_storage,
            state_storage,
            Arc::new(MockApplier::new()),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        )
    }

    /// The `ScriptInstalled` change publishing `installed`.
    fn installing(installed: &InstalledScript) -> Change {
        Change::ScriptInstalled {
            label: installed.label().to_string(),
            script: installed.script().clone(),
            parameters: installed.parameters().clone(),
            folder_id: installed.folder_id(),
        }
    }

    /// Consumes `change` to the installation saved here as `previous`,
    /// expecting it saved as `expected`, with `state_storage` as the
    /// scraper state.
    async fn consume_installation_change(
        previous: Option<InstalledScript>,
        change: Change,
        expected: InstalledScript,
        state_storage: MockStateStorage,
    ) {
        let installation_id = expected.installation_id();
        let event_id = Uuid::now_v7();

        let mut script_storage = MockScriptStorage::new();
        script_storage
            .expect_find_installed_script()
            .withf(move |id| *id == installation_id)
            .times(1)
            .returning(move |_| Ok(previous.clone()));
        script_storage
            .expect_save_installed_script()
            .withf(move |saved| *saved == expected)
            .times(1)
            .returning(|_| Ok(()));

        let client = client_applying_scripts(
            change_proto_event(event_id, installation_id, &change),
            cursor_storage_consuming(event_id),
            script_storage,
            state_storage,
        );

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    /// A scraper state storage expecting `installation_id`'s session to be
    /// cleared, once.
    fn state_storage_clearing(installation_id: Uuid) -> MockStateStorage {
        let mut state_storage = MockStateStorage::new();
        state_storage
            .expect_delete_session()
            .withf(move |name| name == installation_id.to_string())
            .times(1)
            .returning(|_| Ok(()));
        state_storage
    }

    #[tokio::test]
    async fn consume_saves_a_new_installation_and_clears_any_stale_scraper_session() {
        let installed = FakeInstalledScript::new().build();
        let installation_id = installed.installation_id();

        consume_installation_change(
            None,
            installing(&installed),
            installed,
            state_storage_clearing(installation_id),
        )
        .await;
    }

    #[tokio::test]
    async fn consume_clears_the_scraper_session_when_an_installation_is_upgraded() {
        let script = FakeScript::new().with_version(1).build();
        let previous = FakeInstalledScript::new()
            .with_script(script.clone())
            .build();
        let installation_id = previous.installation_id();
        let upgrade = FakeScript::new()
            .with_id(script.id())
            .with_version(2)
            .build();
        let mut expected = previous.clone();
        expected.set_script(upgrade.clone());

        consume_installation_change(
            Some(previous),
            Change::InstallationUpgraded { script: upgrade },
            expected,
            state_storage_clearing(installation_id),
        )
        .await;
    }

    #[tokio::test]
    async fn consume_keeps_the_scraper_session_when_the_same_script_version_is_reinstalled() {
        let script = FakeScript::new().with_version(3).build();
        let previous = FakeInstalledScript::new()
            .with_script(script.clone())
            .build();
        let new = FakeInstalledScript::new()
            .with_installation_id(previous.installation_id())
            .with_script(script)
            .build();

        // No `expect_delete_session`: clearing the session would panic.
        consume_installation_change(
            Some(previous),
            installing(&new),
            new,
            MockStateStorage::new(),
        )
        .await;
    }

    #[tokio::test]
    async fn consume_patches_the_installation_saved_here_with_each_change() {
        let previous = FakeInstalledScript::new()
            .with_label("Marie")
            .with_parameters(HashMap::from([(
                "username".to_string(),
                serde_json::json!("marie"),
            )]))
            .build();
        let parameters =
            HashMap::from([("username".to_string(), serde_json::json!("marie.dupont"))]);
        let folder_id = Uuid::now_v7();
        let mut credentials = previous.clone();
        credentials.set_parameters(parameters.clone());
        let mut renamed = previous.clone();
        renamed.set_label("Compte pro".to_string());
        let mut refiled = previous.clone();
        refiled.set_folder_id(Some(folder_id));

        for (change, expected) in [
            (
                Change::InstallationCredentialsUpdated { parameters },
                credentials,
            ),
            (
                Change::InstallationRenamed {
                    label: "Compte pro".to_string(),
                },
                renamed,
            ),
            (
                Change::InstallationFolderChanged {
                    folder_id: Some(folder_id),
                },
                refiled,
            ),
        ] {
            // No `expect_delete_session`: only an upgrade clears it.
            consume_installation_change(
                Some(previous.clone()),
                change,
                expected,
                MockStateStorage::new(),
            )
            .await;
        }
    }

    #[tokio::test]
    async fn consume_ignores_a_change_to_an_installation_not_saved_here() {
        let installation_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();
        let mut script_storage = MockScriptStorage::new();
        script_storage
            .expect_find_installed_script()
            .times(1)
            .returning(|_| Ok(None));
        // No `expect_save_installed_script`: saving would panic.
        let client = client_applying_scripts(
            change_proto_event(
                event_id,
                installation_id,
                &Change::InstallationRenamed {
                    label: "Compte pro".to_string(),
                },
            ),
            cursor_storage_consuming(event_id),
            script_storage,
            MockStateStorage::new(),
        );

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    #[tokio::test]
    async fn consume_reports_each_installation_event_to_the_callback() {
        let installed = FakeInstalledScript::new().build();
        let installation_id = installed.installation_id();
        let event_id = Uuid::now_v7();
        let mut script_storage = MockScriptStorage::new();
        script_storage
            .expect_find_installed_script()
            .returning(|_| Ok(None));
        script_storage
            .expect_save_installed_script()
            .returning(|_| Ok(()));
        let client = client_applying_scripts(
            change_proto_event(event_id, installation_id, &installing(&installed)),
            cursor_storage_consuming(event_id),
            script_storage,
            state_storage_clearing(installation_id),
        );

        let received = std::sync::Arc::new(Mutex::new(Vec::new()));
        let received_in_callback = received.clone();
        let mut callback = move |event| received_in_callback.lock().unwrap().push(event);
        client.consume_once(&mut callback).await.unwrap();

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].event_type(), EventType::ScriptInstalled);
        assert_eq!(received[0].subject_id(), installation_id);
    }

    #[tokio::test]
    async fn consume_deletes_the_installation_and_its_scraper_state_for_an_uninstall_event() {
        let installation_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();
        let event = change_proto_event(event_id, installation_id, &Change::ScriptUninstalled);

        let mut script_storage = MockScriptStorage::new();
        script_storage
            .expect_delete_installed_script()
            .withf(move |id| *id == installation_id)
            .times(1)
            .returning(|_| Ok(()));
        let mut state_storage = MockStateStorage::new();
        state_storage
            .expect_delete_all()
            .withf(move |name| name == installation_id.to_string())
            .times(1)
            .returning(|_| Ok(()));

        let client = client_applying_scripts(
            event,
            cursor_storage_consuming(event_id),
            script_storage,
            state_storage,
        );

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    /// A changelog client consuming the single `event`, applying folder
    /// events through `folders` and nothing else.
    fn client_applying_folders(
        event: ProtoChangelogEvent,
        cursor_storage: MockCursorStorage,
        folders: MockApplier,
    ) -> ChangelogClient<MockStorage, MockScriptStorage, MockStateStorage, MockCursorStorage> {
        let mut mock_grpc = counting_grpc();
        mock_grpc
            .expect_consume_since()
            .return_once(move |_| Ok(futures::stream::iter(vec![Ok(event)]).boxed()));
        ChangelogClient::new(
            mock_grpc,
            MockStorage::new(),
            idle_indexer(),
            MockScriptStorage::new(),
            MockStateStorage::new(),
            Arc::new(folders),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        )
    }

    #[tokio::test]
    async fn consume_saves_a_created_folder() {
        let folder = FakeFolder::new().build();
        let event_id = Uuid::now_v7();
        let event = change_proto_event(
            event_id,
            folder.id,
            &Change::FolderCreated {
                parent_id: folder.parent_id,
                name: folder.name.clone(),
                appearance: folder.appearance,
            },
        );
        let mut folders = MockApplier::new();
        let expected = folder.clone();
        folders
            .expect_apply_save()
            .withf(move |saved| *saved == expected)
            .times(1)
            .returning(|_| Ok(()));
        let client = client_applying_folders(event, cursor_storage_consuming(event_id), folders);

        let received = std::sync::Arc::new(Mutex::new(Vec::new()));
        let received_in_callback = received.clone();
        let mut callback = move |event| received_in_callback.lock().unwrap().push(event);
        client.consume_once(&mut callback).await.unwrap();

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].event_type(), EventType::FolderCreated);
        assert_eq!(received[0].subject_id(), folder.id);
    }

    #[tokio::test]
    async fn consume_patches_a_folder_with_each_change() {
        let appearance = FolderAppearance {
            color: FolderColor::Teal,
            icon: FolderIcon::Briefcase,
        };
        let parent_id = Uuid::now_v7();
        for (change, patch) in [
            (
                Change::FolderRenamed {
                    name: "Emploi".to_string(),
                },
                FolderPatch::Rename("Emploi".to_string()),
            ),
            (
                Change::FolderAppearanceChanged { appearance },
                FolderPatch::Appearance(appearance),
            ),
            (
                Change::FolderMoved {
                    parent_id: Some(parent_id),
                    appearance: None,
                },
                FolderPatch::Move {
                    parent_id: Some(parent_id),
                    appearance: None,
                },
            ),
        ] {
            let folder_id = Uuid::now_v7();
            let event_id = Uuid::now_v7();
            let mut folders = MockApplier::new();
            folders
                .expect_apply_patch()
                .with(
                    mockall::predicate::eq(folder_id),
                    mockall::predicate::eq(patch),
                )
                .times(1)
                .returning(|_, _| Ok(()));
            let client = client_applying_folders(
                change_proto_event(event_id, folder_id, &change),
                cursor_storage_consuming(event_id),
                folders,
            );

            client.consume_once(&mut |_| {}).await.unwrap();
        }
    }

    #[tokio::test]
    async fn consume_applies_a_delete_folder_event() {
        let folder_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();
        let event = change_proto_event(
            event_id,
            folder_id,
            &Change::FolderDeleted {
                contents: FolderContents::Keep,
            },
        );
        let mut folders = MockApplier::new();
        folders
            .expect_apply_delete()
            .with(
                mockall::predicate::eq(folder_id),
                mockall::predicate::eq(FolderContents::Keep),
            )
            .times(1)
            .returning(|_, _| Ok(()));
        let client = client_applying_folders(event, cursor_storage_consuming(event_id), folders);

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    #[tokio::test]
    async fn consume_deletes_the_cached_document_for_a_deleted_event() {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let mut mock_grpc = counting_grpc();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(change_proto_event(
                event_id,
                document_id,
                &Change::DocumentDeleted,
            ))])
            .boxed())
        });

        // No `expect_prepare()`: a deletion embeds nothing, so it doesn't
        // wait for the indexing model.
        let mut indexer = MockIndexer::new();
        indexer
            .expect_delete_document()
            .withf(move |id| *id == document_id)
            .times(1)
            .returning(|_| Ok(()));

        // No expectations: the document is deleted through the indexer.
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
            Arc::new(indexer),
            MockScriptStorage::new(),
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        let mut received = Vec::new();
        client
            .consume_once(&mut |event| received.push(event))
            .await
            .unwrap();

        assert_eq!(received.len(), 1);
        assert_eq!(received[0].event_type(), EventType::DocumentDeleted);
        assert_eq!(received[0].subject_id, document_id);
    }

    #[tokio::test]
    async fn consume_does_not_advance_past_a_deletion_it_failed_to_apply() {
        let event_id = Uuid::now_v7();

        let mut mock_grpc = counting_grpc();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(change_proto_event(
                event_id,
                Uuid::now_v7(),
                &Change::DocumentDeleted,
            ))])
            .boxed())
        });

        let mut indexer = MockIndexer::new();
        indexer
            .expect_delete_document()
            .returning(|_| Err(ErrorKind::InvalidChangelogEvent("disk full".into()).into()));

        // No `expect_mark_applied()`/`expect_save_cursor()`: the cursor
        // stays before the event, so the next pass retries it.
        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage.expect_is_applied().returning(|_| Ok(false));
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));

        let client = ChangelogClient::new(
            mock_grpc,
            MockStorage::new(),
            Arc::new(indexer),
            MockScriptStorage::new(),
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        let err = client.consume_once(&mut |_| {}).await.unwrap_err();

        assert!(err.has_context(), "missing context: {err}");
    }

    #[tokio::test]
    async fn consume_patches_the_cached_documents_metadata_with_each_change() {
        let folder_id = Uuid::now_v7();
        let attributes = BTreeMap::from([("amount".to_string(), Attribute::Integer(42))]);
        let cached = FakeMetadata::new()
            .with_name("old.pdf")
            .with_folder(None)
            .build();
        for (change, expected) in [
            (
                renaming(),
                Metadata {
                    name: "renamed.pdf".to_string(),
                    ..cached.clone()
                },
            ),
            (
                Change::DocumentAttributesUpdated {
                    r#type: Some(DocumentType::Invoice),
                    attributes: attributes.clone(),
                },
                Metadata {
                    r#type: Some(DocumentType::Invoice),
                    attributes: attributes.clone(),
                    ..cached.clone()
                },
            ),
            (
                Change::DocumentMoved {
                    folder_id: Some(folder_id),
                },
                Metadata {
                    folder_id: Some(folder_id),
                    ..cached.clone()
                },
            ),
        ] {
            let document_id = cached.id;
            let event_id = Uuid::now_v7();

            let mut document_storage = MockStorage::new();
            let held = cached.clone();
            document_storage
                .expect_get_metadata()
                .withf(move |id| *id == document_id)
                .times(1)
                .returning(move |_| Ok(Some(held.clone())));
            let mut indexer = ready_indexer();
            indexer
                .expect_update_metadata()
                .withf(move |id, metadata| *id == document_id && *metadata == expected)
                .times(1)
                .returning(|_, _| Ok(()));

            let client = ChangelogClient::new(
                grpc_streaming(change_proto_event(event_id, document_id, &change)),
                document_storage,
                Arc::new(indexer),
                MockScriptStorage::new(),
                MockStateStorage::new(),
                Arc::new(MockApplier::new()),
                cursor_storage_consuming(event_id),
                reachable_server_state(),
                fake_secrets(),
            );

            client.consume_once(&mut |_| {}).await.unwrap();
        }
    }

    #[tokio::test]
    async fn consume_ignores_a_change_to_a_document_not_cached_here() {
        let document_id = Uuid::now_v7();
        let event_id = Uuid::now_v7();
        let mut document_storage = MockStorage::new();
        document_storage
            .expect_get_metadata()
            .times(1)
            .returning(|_| Ok(None));

        // No `expect_update_metadata`: updating would panic.
        let client = ChangelogClient::new(
            grpc_streaming(change_proto_event(event_id, document_id, &renaming())),
            document_storage,
            Arc::new(ready_indexer()),
            MockScriptStorage::new(),
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
            cursor_storage_consuming(event_id),
            reachable_server_state(),
            fake_secrets(),
        );

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    #[tokio::test]
    async fn consume_resumes_from_the_persisted_cursor() {
        let document_id = Uuid::now_v7();
        let cursor = Uuid::now_v7();
        let event_id = Uuid::now_v7();

        let mut mock_grpc = counting_grpc();
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
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        client.consume_once(&mut |_| {}).await.unwrap();
    }

    /// A client streaming `events` from the beginning (nil cursor), caching
    /// every `DocumentUploaded` event that gets that far as a new document.
    fn client_streaming(
        events: Vec<ProtoChangelogEvent>,
    ) -> ChangelogClient<MockStorage, MockScriptStorage, MockStateStorage, MockCursorStorage> {
        client_streaming_from(counting_grpc(), events)
    }

    /// [`client_streaming`] over `mock_grpc`, which must answer
    /// `count_since` itself.
    fn client_streaming_from(
        mut mock_grpc: MockFydeClient,
        events: Vec<ProtoChangelogEvent>,
    ) -> ChangelogClient<MockStorage, MockScriptStorage, MockStateStorage, MockCursorStorage> {
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
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        )
    }

    /// Two chained events, as [`client_streaming`] streams them.
    fn two_chained_events() -> Vec<ProtoChangelogEvent> {
        let (first_id, second_id) = (Uuid::now_v7(), Uuid::now_v7());
        vec![
            proto_event(first_id, Uuid::now_v7()),
            ProtoChangelogEvent {
                previous_id: first_id.to_string(),
                ..proto_event(second_id, Uuid::now_v7())
            },
        ]
    }

    /// A [`MockFydeClient`] whose server counts `count` events from the
    /// beginning of the changelog.
    fn grpc_counting(count: u64) -> MockFydeClient {
        let mut mock = MockFydeClient::new();
        mock.expect_count_since()
            .withf(|id| id.is_none())
            .returning(move |_| Ok(count));
        mock
    }

    #[tokio::test]
    async fn sync_progress_counts_consumed_events_against_the_servers_count() {
        let client = client_streaming_from(grpc_counting(3), two_chained_events());

        client.consume_once(&mut |_| {}).await.unwrap();

        let progress = client.sync_progress();
        assert_eq!(
            progress,
            SyncProgress {
                consumed: 2,
                total: Some(3)
            }
        );
        assert!(!progress.is_caught_up());
    }

    #[tokio::test]
    async fn sync_progress_raises_the_total_for_events_recorded_after_the_count() {
        let client = client_streaming_from(grpc_counting(1), two_chained_events());

        client.consume_once(&mut |_| {}).await.unwrap();

        let progress = client.sync_progress();
        assert_eq!(
            progress,
            SyncProgress {
                consumed: 2,
                total: Some(2)
            }
        );
        assert!(progress.is_caught_up());
    }

    #[tokio::test]
    async fn sync_progress_is_caught_up_at_once_when_nothing_is_left_to_consume() {
        let client = client_streaming_from(grpc_counting(0), Vec::new());

        client.consume_once(&mut |_| {}).await.unwrap();

        assert!(client.sync_progress().is_caught_up());
    }

    #[tokio::test]
    async fn sync_progress_adds_a_later_passs_count_to_what_was_consumed() {
        let mut mock_grpc = MockFydeClient::new();
        let mut counts = vec![2, 1].into_iter();
        mock_grpc
            .expect_count_since()
            .returning(move |_| Ok(counts.next().unwrap()));
        let client = client_streaming_from(mock_grpc, two_chained_events());

        client.consume_once(&mut |_| {}).await.unwrap();
        client.consume_once(&mut |_| {}).await.unwrap();

        // 2 consumed, then 1 more counted: 3, until the second pass's two
        // events raise it.
        assert_eq!(
            client.sync_progress(),
            SyncProgress {
                consumed: 4,
                total: Some(4)
            }
        );
    }

    #[tokio::test]
    async fn sync_progress_has_no_total_but_consumes_when_the_server_cannot_count() {
        let mut mock_grpc = MockFydeClient::new();
        mock_grpc.expect_count_since().returning(|_| {
            Err(ErrorKind::InvalidResponse("CountSince is not implemented".into()).into())
        });
        let client = client_streaming_from(mock_grpc, two_chained_events());

        let mut consumed = 0;
        client.consume_once(&mut |_| consumed += 1).await.unwrap();

        assert_eq!(consumed, 2);
        let progress = client.sync_progress();
        assert_eq!(
            progress,
            SyncProgress {
                consumed: 2,
                total: None
            }
        );
        assert!(!progress.is_caught_up());
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
        let mut mock_grpc = counting_grpc();
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
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        let mut callback = |_| {};
        let consume_fut = client.run_consume_loop(0, &mut callback);
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
        let mut mock_grpc = counting_grpc();
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
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        let mut callback = |_| {};
        let consume_fut = client.run_consume_loop(0, &mut callback);
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
    ) -> ChangelogClient<MockStorage, MockScriptStorage, MockStateStorage, MockCursorStorage> {
        let mut mock_grpc = counting_grpc();
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
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
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
        let consume_fut = client.run_consume_loop(0, &mut callback);
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
            tokio::spawn(async move { loop_client.run_consume_loop(0, &mut |_| {}).await });

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
        let mut mock_grpc = counting_grpc();
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
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        ));

        let loop_client = client.clone();
        let loop_task =
            tokio::spawn(async move { loop_client.run_consume_loop(0, &mut |_| {}).await });

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
        let mock_grpc = counting_grpc();
        let document_storage = MockStorage::new();
        let cursor_storage = MockCursorStorage::new();

        let client = ChangelogClient::new(
            mock_grpc,
            document_storage,
            idle_indexer(),
            MockScriptStorage::new(),
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        );

        // The job the loop runs as, stopped before it starts.
        let generation = *client.job.borrow();
        client.stop_consume_job();

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.run_consume_loop(generation, &mut |_| {}),
        )
        .await
        .expect("run_consume_loop did not return promptly");
    }

    #[tokio::test]
    async fn start_consume_job_returns_as_soon_as_the_loop_is_spawned() {
        // The stream never yields anything and never closes, so if
        // `start_consume_job` awaited the loop inline instead of spawning
        // it onto its own task, this test would time out.
        let mut mock_grpc = counting_grpc();
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
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
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

    /// A job stopped then started again before its task ever ran (as at a
    /// logout followed by a login) must not resume beside the new one: two
    /// tasks would apply the same events at once.
    #[tokio::test]
    async fn start_consume_job_right_after_a_stop_leaves_only_the_new_job_consuming() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let streams_opened = Arc::new(AtomicU32::new(0));
        let mut mock_grpc = counting_grpc();
        let opened = streams_opened.clone();
        mock_grpc.expect_consume_since().returning(move |_| {
            opened.fetch_add(1, Ordering::SeqCst);
            Ok(futures::stream::pending().boxed())
        });

        let mut cursor_storage = MockCursorStorage::new();
        cursor_storage
            .expect_get_cursor()
            .returning(|| Ok(Uuid::nil()));

        let client = Arc::new(ChangelogClient::new(
            mock_grpc,
            MockStorage::new(),
            idle_indexer(),
            MockScriptStorage::new(),
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        ));

        // `#[tokio::test]`'s single-threaded runtime runs neither spawned
        // task until this test yields.
        client
            .clone()
            .start_consume_job(Box::new(|_| {}))
            .await
            .unwrap();
        client.stop_consume_job();
        client
            .clone()
            .start_consume_job(Box::new(|_| {}))
            .await
            .unwrap();

        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(streams_opened.load(Ordering::SeqCst), 1);

        client.stop_consume_job();
    }

    #[tokio::test]
    async fn send_encrypts_the_event_before_it_reaches_the_transport_layer() {
        let document_id = Uuid::now_v7();
        let metadata = FakeMetadata::new().build();

        let mut mock_grpc = counting_grpc();
        let expected_metadata = metadata.clone();
        mock_grpc
            .expect_record_event()
            .withf(move |encrypted_content| {
                let decrypted = futures::executor::block_on(crypto::decrypt_event(
                    &*fake_secrets(),
                    encrypted_content,
                ))
                .unwrap();
                decrypted
                    == (
                        document_id,
                        Change::DocumentUploaded {
                            content: b"body".to_vec(),
                            metadata: expected_metadata.clone(),
                        },
                    )
            })
            .returning(|_| Ok(Uuid::now_v7()));

        let client = ChangelogClient::new(
            mock_grpc,
            MockStorage::new(),
            idle_indexer(),
            MockScriptStorage::new(),
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
            MockCursorStorage::new(),
            reachable_server_state(),
            fake_secrets(),
        );

        client
            .send(
                document_id,
                &Change::DocumentUploaded {
                    content: b"body".to_vec(),
                    metadata,
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn send_refuses_an_event_larger_than_the_server_accepts() {
        // No `expect_record_event()`: the mock panics if it reaches the
        // transport layer.
        let client = ChangelogClient::new(
            counting_grpc(),
            MockStorage::new(),
            idle_indexer(),
            MockScriptStorage::new(),
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
            MockCursorStorage::new(),
            reachable_server_state(),
            fake_secrets(),
        );

        let err = client
            .send(
                Uuid::now_v7(),
                &Change::DocumentUploaded {
                    content: vec![0; super::super::MAX_EVENT_BYTES],
                    metadata: FakeMetadata::new().build(),
                },
            )
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::DocumentTooLarge);
    }

    /// A client streaming `events` from the beginning (nil cursor) to
    /// `indexer`, with no duplicate of an upload cached and every document
    /// a change is about cached, which must not apply (nor skip) any of
    /// them.
    fn client_applying_nothing(
        events: Vec<ProtoChangelogEvent>,
        indexer: MockIndexer,
    ) -> ChangelogClient<MockStorage, MockScriptStorage, MockStateStorage, MockCursorStorage> {
        let mut mock_grpc = counting_grpc();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(events.clone().into_iter().map(Ok)).boxed())
        });
        let mut document_storage = MockStorage::new();
        document_storage
            .expect_find_document_by_checksum()
            .returning(|_| Ok(None));
        document_storage
            .expect_get_metadata()
            .returning(|id| Ok(Some(FakeMetadata::new().with_id(id).build())));

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
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
            cursor_storage,
            reachable_server_state(),
            fake_secrets(),
        )
    }

    #[tokio::test]
    async fn consume_prepares_the_indexing_model_once_per_pass_before_the_first_document() {
        let (first_id, second_id) = (Uuid::now_v7(), Uuid::now_v7());
        let first = change_proto_event(first_id, Uuid::now_v7(), &renaming());
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

        let mut mock_grpc = counting_grpc();
        mock_grpc.expect_consume_since().returning(move |_| {
            Ok(futures::stream::iter(vec![Ok(first.clone()), Ok(second.clone())]).boxed())
        });
        let mut document_storage = MockStorage::new();
        document_storage
            .expect_find_document_by_checksum()
            .returning(|_| Ok(None));
        document_storage
            .expect_get_metadata()
            .returning(|id| Ok(Some(FakeMetadata::new().with_id(id).build())));
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
            MockStateStorage::new(),
            Arc::new(MockApplier::new()),
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
            vec![change_proto_event(
                Uuid::now_v7(),
                Uuid::now_v7(),
                &renaming(),
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
            MockStateStorage,
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
