//! The on-device model files the SDK can run (see `tools::onnx`), each pinned
//! to a fixed upstream revision, size and SHA-256, and [`ModelStore`], which
//! downloads them into `ClientConfig::models_dir`.
//!
//! That directory is never the client's own one: logout deletes every file
//! there, and models aren't user data. Nothing downloads on `Client::init`:
//! only [`ModelStore::ensure`] does, when a model is first needed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::StreamExt as _;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt as _;

use crate::error::{ErrorContext, ErrorKind, Result};
use crate::tools::hex;

/// One pinned model file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Model {
    /// Its file name in the models directory, after a prefix of its SHA-256
    /// (see [`ModelStore::path`]). Keeps the upstream extension, which ONNX
    /// Runtime tells formats apart by.
    pub(crate) name: &'static str,
    /// Where to download it from, at a fixed revision.
    pub(crate) url: &'static str,
    /// Its exact size in bytes.
    pub(crate) size: u64,
    /// Its SHA-256, lowercase hex.
    pub(crate) sha256: &'static str,
}

macro_rules! gliner_x_small_url {
    ($file:literal) => {
        concat!(
            "https://huggingface.co/knowledgator/gliner-x-small/resolve/",
            "d51a0984d11084a55f9df3899d9dbf7704f580f5/",
            $file
        )
    };
}

/// GLiNER-X small (Apache-2.0, mT5-small encoder), int8-quantized:
/// multilingual zero-shot entity spans (see `documents::gliner`).
pub(crate) const GLINER_X_SMALL: Model = Model {
    name: "gliner-x-small.onnx",
    url: gliner_x_small_url!("onnx/model_quantized.onnx"),
    size: 172_716_978,
    sha256: "d76b3349fd4bfe44573221c0912ed459f432d14317552fdc65935dabcb833c4a",
};

/// [`GLINER_X_SMALL`]'s tokenizer.
pub(crate) const GLINER_X_SMALL_TOKENIZER: Model = Model {
    name: "gliner-x-small.tokenizer.json",
    url: gliner_x_small_url!("tokenizer.json"),
    size: 16_335_050,
    sha256: "619d3790cf59a0e50afdf04239db19093568fbd8d806963e35d54a8fb71d099c",
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a download may go without receiving a byte before it fails.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// A partial download untouched for this long was abandoned (its process
/// died before cleaning it up): an active one writes at least every
/// [`READ_TIMEOUT`].
const STALE_PARTIAL_AGE: Duration = Duration::from_secs(10 * 60);
const PARTIAL_SUFFIX: &str = ".part";
/// The length of the SHA-256 prefix in a model's file name.
const HASH_PREFIX_LEN: usize = 16;

/// Downloads [`Model`]s into a directory, verified and atomically.
pub(crate) struct ModelStore {
    dir: PathBuf,
    /// Built on the first download, so a store that never downloads (most
    /// `Client`s, most of the time) never sets up TLS.
    http: tokio::sync::OnceCell<wreq::Client>,
    /// One lock per model file, held across checking and downloading it so
    /// concurrent [`ModelStore::ensure`] calls download it once. Holds
    /// whether this store already checked the file's SHA-256.
    files: Mutex<HashMap<String, Arc<tokio::sync::Mutex<bool>>>>,
}

impl ModelStore {
    /// Doesn't touch `dir`: [`ModelStore::ensure`] creates it when needed.
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            http: tokio::sync::OnceCell::new(),
            files: Mutex::default(),
        }
    }

    /// Where `model` lives once downloaded. The SHA-256 prefix keeps a
    /// re-pinned model from ever being mistaken for its previous revision.
    pub(crate) fn path(&self, model: &Model) -> PathBuf {
        self.dir.join(file_name(model))
    }

    /// Returns `model`'s local path, downloading it first unless it's
    /// already there. A file already there is checked against the pinned
    /// SHA-256 the first time this store sees it (replaced if it doesn't
    /// match), then only by size. A download goes to a temporary file in
    /// the same directory, is checked against the pinned size and SHA-256
    /// (failing with `ModelCorrupted` otherwise), then renamed into place:
    /// a failed, mismatched or cancelled download never leaves a file at
    /// that path, and removes its temporary one. Downloading also removes
    /// `model`'s previous revisions and abandoned partial downloads.
    pub(crate) async fn ensure(&self, model: &Model) -> Result<PathBuf> {
        let path = self.path(model);
        let lock = self.file_lock(model);
        let mut verified = lock.lock().await;

        if let Ok(metadata) = tokio::fs::metadata(&path).await
            && metadata.len() == model.size
        {
            if *verified {
                return Ok(path);
            }
            let sha256 = sha256_of(&path)
                .await
                .with_context(|| format!("failed to check the model {}", model.name))?;
            if sha256 == model.sha256 {
                *verified = true;
                return Ok(path);
            }
            remove_if_present(&path)
                .await
                .with_context(|| format!("failed to discard the corrupted model {}", model.name))?;
        }

        tokio::fs::create_dir_all(&self.dir)
            .await
            .with_context(|| {
                format!(
                    "failed to create the models directory {}",
                    self.dir.display()
                )
            })?;
        self.remove_leftovers(model)
            .await
            .with_context(|| format!("failed to clean up old downloads of {}", model.name))?;

        let mut partial = PartialFile {
            path: self.dir.join(format!(
                ".{}.{}{PARTIAL_SUFFIX}",
                file_name(model),
                uuid::Uuid::new_v4()
            )),
            renamed: false,
        };
        self.download(model, &partial.path)
            .await
            .with_context(|| format!("failed to download the model {}", model.name))?;
        tokio::fs::rename(&partial.path, &path)
            .await
            .with_context(|| format!("failed to move the model {} into place", model.name))?;
        partial.renamed = true;
        *verified = true;

        Ok(path)
    }

    fn file_lock(&self, model: &Model) -> Arc<tokio::sync::Mutex<bool>> {
        self.files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(file_name(model))
            .or_default()
            .clone()
    }

    async fn http(&self) -> Result<&wreq::Client> {
        self.http
            .get_or_try_init(|| async {
                wreq::Client::builder()
                    // Hugging Face redirects every download to its CDN.
                    .redirect(wreq::redirect::Policy::default())
                    .connect_timeout(CONNECT_TIMEOUT)
                    .read_timeout(READ_TIMEOUT)
                    .build()
                    .map_err(ErrorKind::ModelDownload)
                    .context("failed to build the model download HTTP client")
            })
            .await
    }

    async fn download(&self, model: &Model, to: &Path) -> Result<()> {
        let response = self
            .http()
            .await?
            .get(model.url)
            .send()
            .await
            .map_err(ErrorKind::ModelDownload)
            .with_context(|| format!("failed to request {}", model.url))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ErrorKind::ModelDownloadStatus(status.as_u16()).into());
        }

        let mut file = tokio::fs::File::create(to)
            .await
            .context("failed to create the download's temporary file")?;
        let mut hasher = Sha256::new();
        let mut received: u64 = 0;
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk
                .map_err(ErrorKind::ModelDownload)
                .context("failed to receive the download")?;
            received += chunk.len() as u64;
            if received > model.size {
                return Err(ErrorKind::ModelCorrupted(format!(
                    "more than the expected {} bytes",
                    model.size
                ))
                .into());
            }
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .context("failed to write the download to its temporary file")?;
        }
        file.sync_all()
            .await
            .context("failed to flush the download to disk")?;

        if received != model.size {
            return Err(ErrorKind::ModelCorrupted(format!(
                "{received} bytes instead of {}",
                model.size
            ))
            .into());
        }
        let sha256 = hex::encode(&hasher.finalize());
        if sha256 != model.sha256 {
            return Err(ErrorKind::ModelCorrupted(format!(
                "SHA-256 {sha256} instead of {}",
                model.sha256
            ))
            .into());
        }

        Ok(())
    }

    /// Removes what earlier downloads of `model` left behind: the files of
    /// its previous revisions (re-pinned since), and its partial downloads,
    /// of any revision, nothing has written to for [`STALE_PARTIAL_AGE`]
    /// (keeping those another download, in this process or another sharing
    /// the directory, may still be writing).
    async fn remove_leftovers(&self, model: &Model) -> Result<()> {
        let current = file_name(model);
        let mut entries = tokio::fs::read_dir(&self.dir)
            .await
            .context("failed to list the models directory")?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .context("failed to list the models directory")?
        {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(target) = partial_target(&name)
                && is_revision_of(target, model)
            {
                remove_if_stale(&entry.path())
                    .await
                    .with_context(|| format!("failed to clean up the partial download {name}"))?;
            } else if is_revision_of(&name, model) && name != current {
                remove_if_present(&entry.path())
                    .await
                    .with_context(|| format!("failed to remove the previous revision {name}"))?;
            }
        }

        Ok(())
    }
}

pub(crate) fn file_name(model: &Model) -> String {
    format!("{}-{}", &model.sha256[..HASH_PREFIX_LEN], model.name)
}

/// Whether `name` is the file name of `model` at any revision: its name
/// after any SHA-256 prefix (see [`file_name`]).
fn is_revision_of(name: &str, model: &Model) -> bool {
    name.split_once('-').is_some_and(|(prefix, rest)| {
        rest == model.name
            && prefix.len() == HASH_PREFIX_LEN
            && prefix
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// The file name the partial download `name` is to be renamed to, if `name`
/// is one (`.<file name>.<id>.part`).
fn partial_target(name: &str) -> Option<&str> {
    let (target, _id) = name
        .strip_prefix('.')?
        .strip_suffix(PARTIAL_SUFFIX)?
        .rsplit_once('.')?;
    Some(target)
}

/// The SHA-256 of the file at `path`, lowercase hex, read off the runtime's
/// threads (a model is hundreds of megabytes).
async fn sha256_of(path: &Path) -> Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0; 1 << 16];
        loop {
            let read = std::io::Read::read(&mut file, &mut buffer)
                .with_context(|| format!("failed to read {}", path.display()))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hex::encode(&hasher.finalize()))
    })
    .await
    .context("failed to hash a model file")?
}

/// Removes the file at `path`, if it's still there: another process sharing
/// the directory may have removed it first.
async fn remove_if_present(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            Err(err).with_context(|| format!("failed to remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

/// Removes the file at `path` if nothing has written to it for
/// [`STALE_PARTIAL_AGE`], or nothing if it's gone already.
async fn remove_if_stale(path: &Path) -> Result<()> {
    let modified = match tokio::fs::metadata(path)
        .await
        .and_then(|metadata| metadata.modified())
    {
        Ok(modified) => modified,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to read the age of {}", path.display()));
        }
    };
    let age = SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default();
    if age >= STALE_PARTIAL_AGE {
        remove_if_present(path)
            .await
            .context("failed to remove a stale file")?;
    }

    Ok(())
}

/// Deletes the partial download at `path` when dropped (on failure or
/// cancellation), unless it was `renamed` into place.
struct PartialFile {
    path: PathBuf,
    renamed: bool,
}

impl Drop for PartialFile {
    fn drop(&mut self) {
        if !self.renamed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;
    use crate::tools::testing::{random_bytes, random_hex};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncReadExt as _;

    /// What the test server does once it wrote a reply.
    #[derive(Clone, Copy)]
    enum Then {
        Close,
        /// Keeps the connection open without sending anything more.
        Hang,
    }

    struct Reply {
        head: String,
        body: Vec<u8>,
        then: Then,
    }

    impl Reply {
        fn ok(body: &[u8]) -> Self {
            Self::claiming(body.len(), body)
        }

        /// A 200 announcing `content_length` bytes, whatever `body` holds.
        fn claiming(content_length: usize, body: &[u8]) -> Self {
            Self {
                head: format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n"
                ),
                body: body.to_vec(),
                then: Then::Close,
            }
        }

        fn status(status: &str) -> Self {
            Self {
                head: format!(
                    "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                ),
                body: Vec::new(),
                then: Then::Close,
            }
        }

        fn redirect(location: &str) -> Self {
            Self {
                head: format!(
                    "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                ),
                body: Vec::new(),
                then: Then::Close,
            }
        }

        fn then(mut self, then: Then) -> Self {
            self.then = then;
            self
        }
    }

    struct Server {
        url: String,
        requests: Arc<AtomicUsize>,
    }

    /// Serves every request on the current runtime with `reply(path)`.
    async fn serve(reply: impl Fn(&str) -> Reply + Send + Sync + 'static) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let reply = Arc::new(reply);

        let counter = requests.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let reply = reply.clone();
                tokio::spawn(async move {
                    let mut request = [0u8; 4096];
                    let read = socket.read(&mut request).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&request[..read]);
                    let path = request.split(' ').nth(1).unwrap_or("/");
                    let reply = reply(path);

                    let _ = socket.write_all(reply.head.as_bytes()).await;
                    let _ = socket.write_all(&reply.body).await;
                    match reply.then {
                        Then::Close => {
                            let _ = socket.shutdown().await;
                        }
                        Then::Hang => std::future::pending::<()>().await,
                    }
                });
            }
        });

        Server { url, requests }
    }

    fn leak(value: String) -> &'static str {
        Box::leak(value.into_boxed_str())
    }

    /// A model of `content`, served at `path` on `server`.
    fn model(server: &Server, path: &str, content: &[u8]) -> Model {
        Model {
            name: leak(format!("{}.onnx", random_hex(8))),
            url: leak(format!("{}{path}", server.url)),
            size: content.len() as u64,
            sha256: leak(hex::encode(&Sha256::digest(content))),
        }
    }

    fn content() -> Vec<u8> {
        // Large enough to arrive in several chunks.
        random_bytes(256 * 1024)
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut files: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        files
    }

    #[tokio::test]
    async fn ensure_downloads_a_model() {
        let content = content();
        let served = content.clone();
        let server = serve(move |_| Reply::ok(&served)).await;
        let model = model(&server, "/model.onnx", &content);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());

        let path = store.ensure(&model).await.unwrap();

        assert_eq!(path, store.path(&model));
        assert_eq!(std::fs::read(&path).unwrap(), content);
        assert_eq!(files_in(dir.path()), vec![file_name(&model)]);
    }

    #[tokio::test]
    async fn ensure_follows_redirects() {
        let content = content();
        let served = content.clone();
        let server = serve(move |path| match path {
            "/resolve/model.onnx" => Reply::redirect("/cdn/model.onnx"),
            "/cdn/model.onnx" => Reply::ok(&served),
            _ => Reply::status("404 Not Found"),
        })
        .await;
        let model = model(&server, "/resolve/model.onnx", &content);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());

        let path = store.ensure(&model).await.unwrap();

        assert_eq!(std::fs::read(path).unwrap(), content);
        assert_eq!(server.requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn ensure_creates_a_missing_models_dir() {
        let content = content();
        let served = content.clone();
        let server = serve(move |_| Reply::ok(&served)).await;
        let model = model(&server, "/model.onnx", &content);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("not").join("there");
        let store = ModelStore::new(dir.clone());

        let path = store.ensure(&model).await.unwrap();

        assert!(path.starts_with(&dir));
        assert_eq!(std::fs::read(path).unwrap(), content);
    }

    #[tokio::test]
    async fn ensure_skips_a_model_already_present() {
        let content = content();
        let served = content.clone();
        let server = serve(move |_| Reply::ok(&served)).await;
        let model = model(&server, "/model.onnx", &content);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());

        let first = store.ensure(&model).await.unwrap();
        let second = store.ensure(&model).await.unwrap();

        assert_eq!(first, second);
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ensure_replaces_a_present_file_of_the_wrong_size() {
        let content = content();
        let served = content.clone();
        let server = serve(move |_| Reply::ok(&served)).await;
        let model = model(&server, "/model.onnx", &content);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());
        std::fs::write(store.path(&model), b"truncated by a crash").unwrap();

        let path = store.ensure(&model).await.unwrap();

        assert_eq!(std::fs::read(path).unwrap(), content);
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ensure_rejects_a_hash_mismatch_and_leaves_no_file() {
        let content = content();
        let mut tampered = content.clone();
        tampered[0] ^= 0xff;
        let server = serve(move |_| Reply::ok(&tampered)).await;
        let model = model(&server, "/model.onnx", &content);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());

        let err = store.ensure(&model).await.unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelCorrupted);
        assert!(err.has_context());
        assert!(err.to_string().contains("SHA-256"), "{err}");
        assert!(files_in(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn ensure_rejects_a_shorter_file_and_leaves_no_file() {
        let content = content();
        let short = content[..content.len() / 2].to_vec();
        let server = serve(move |_| Reply::ok(&short)).await;
        let model = model(&server, "/model.onnx", &content);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());

        let err = store.ensure(&model).await.unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelCorrupted);
        assert!(files_in(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn ensure_stops_a_download_larger_than_pinned() {
        let content = content();
        let mut longer = content.clone();
        longer.extend_from_slice(&content);
        // Then hangs: only stopping as soon as the size is exceeded returns.
        let server =
            serve(move |_| Reply::claiming(longer.len() * 2, &longer).then(Then::Hang)).await;
        let model = model(&server, "/model.onnx", &content);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());

        let err = store.ensure(&model).await.unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelCorrupted);
        assert!(err.to_string().contains("more than"), "{err}");
        assert!(files_in(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn ensure_leaves_no_partial_file_when_the_connection_drops() {
        let content = content();
        let half = content[..content.len() / 2].to_vec();
        let size = content.len();
        let server = serve(move |_| Reply::claiming(size, &half)).await;
        let model = model(&server, "/model.onnx", &content);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());

        let err = store.ensure(&model).await.unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelDownloadFailed);
        assert!(err.has_context());
        assert!(files_in(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn ensure_leaves_no_partial_file_when_cancelled() {
        let content = content();
        let half = content[..content.len() / 2].to_vec();
        let size = content.len();
        let server = serve(move |_| Reply::claiming(size, &half).then(Then::Hang)).await;
        let model = model(&server, "/model.onnx", &content);
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ModelStore::new(dir.path().to_path_buf()));

        let download = tokio::spawn({
            let store = store.clone();
            async move { store.ensure(&model).await }
        });
        let partial_written = async {
            loop {
                let written = std::fs::read_dir(dir.path())
                    .unwrap()
                    .any(|entry| entry.unwrap().metadata().unwrap().len() > 0);
                if written {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), partial_written)
            .await
            .expect("the download never started writing");
        download.abort();
        assert!(download.await.unwrap_err().is_cancelled());

        assert!(files_in(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn ensure_fails_on_a_missing_model_and_leaves_no_file() {
        let server = serve(|_| Reply::status("404 Not Found")).await;
        let model = model(&server, "/model.onnx", &content());
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());

        let err = store.ensure(&model).await.unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelUnavailable);
        assert!(err.has_context());
        assert!(err.to_string().contains("404"), "{err}");
        assert!(files_in(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn ensure_fails_on_a_server_error_and_leaves_no_file() {
        let server = serve(|_| Reply::status("503 Service Unavailable")).await;
        let model = model(&server, "/model.onnx", &content());
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());

        let err = store.ensure(&model).await.unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelDownloadFailed);
        assert!(err.has_context());
        assert!(err.to_string().contains("503"), "{err}");
        assert!(files_in(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn ensure_fails_when_the_server_is_unreachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let unreachable = Server {
            url: format!("http://{}", listener.local_addr().unwrap()),
            requests: Arc::new(AtomicUsize::new(0)),
        };
        drop(listener);
        let model = model(&unreachable, "/model.onnx", &content());
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());

        let err = store.ensure(&model).await.unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelDownloadFailed);
        assert!(err.has_context());
    }

    #[tokio::test]
    async fn ensure_removes_only_stale_partial_downloads() {
        let content = content();
        let served = content.clone();
        let server = serve(move |_| Reply::ok(&served)).await;
        let model = model(&server, "/model.onnx", &content);
        let previous = Model {
            sha256: leak(random_hex(32)),
            ..model
        };
        let other = Model {
            name: "other.onnx",
            ..model
        };
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());
        let partial =
            |model: &Model, id: &str| format!(".{}.{id}{PARTIAL_SUFFIX}", file_name(model));
        let long_ago = SystemTime::now() - STALE_PARTIAL_AGE - Duration::from_secs(1);
        for (name, modified) in [
            (partial(&model, "stale"), Some(long_ago)),
            (partial(&model, "active"), None),
            (partial(&previous, "stale"), Some(long_ago)),
            (partial(&previous, "active"), None),
            (partial(&other, "stale"), Some(long_ago)),
        ] {
            let file = std::fs::File::create(dir.path().join(name)).unwrap();
            if let Some(modified) = modified {
                file.set_modified(modified).unwrap();
            }
        }

        store.ensure(&model).await.unwrap();

        let mut expected = vec![
            file_name(&model),
            partial(&model, "active"),
            partial(&previous, "active"),
            partial(&other, "stale"),
        ];
        expected.sort();
        assert_eq!(files_in(dir.path()), expected);
    }

    #[tokio::test]
    async fn ensure_removes_the_previous_revisions_of_a_model_it_downloads() {
        let content = content();
        let served = content.clone();
        let server = serve(move |_| Reply::ok(&served)).await;
        let model = model(&server, "/model.onnx", &content);
        let previous = Model {
            sha256: leak(random_hex(32)),
            ..model
        };
        let other = Model {
            name: "other.onnx",
            ..model
        };
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());
        let unrelated = [
            file_name(&other),
            format!("not-a-hash-{}", model.name),
            format!("{}-{}", "F".repeat(HASH_PREFIX_LEN), model.name),
            model.name.to_string(),
        ];
        for name in unrelated.iter().chain([&file_name(&previous)]) {
            std::fs::write(dir.path().join(name), b"kept or not").unwrap();
        }

        store.ensure(&model).await.unwrap();

        let mut expected = unrelated.to_vec();
        expected.push(file_name(&model));
        expected.sort();
        assert_eq!(files_in(dir.path()), expected);
    }

    #[tokio::test]
    async fn ensure_trusts_a_correct_file_already_there_without_downloading() {
        let content = content();
        let server = serve(|_| Reply::status("500 Internal Server Error")).await;
        let model = model(&server, "/model.onnx", &content);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());
        std::fs::write(store.path(&model), &content).unwrap();

        let path = store.ensure(&model).await.unwrap();

        assert_eq!(path, store.path(&model));
        assert_eq!(server.requests.load(Ordering::SeqCst), 0);
        assert!(store.http.get().is_none());
    }

    #[tokio::test]
    async fn ensure_replaces_a_file_of_the_right_size_but_wrong_content() {
        let content = content();
        let served = content.clone();
        let server = serve(move |_| Reply::ok(&served)).await;
        let model = model(&server, "/model.onnx", &content);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());
        let mut corrupted = content.clone();
        corrupted[content.len() / 2] ^= 0xff;
        std::fs::write(store.path(&model), &corrupted).unwrap();

        let path = store.ensure(&model).await.unwrap();

        assert_eq!(std::fs::read(path).unwrap(), content);
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ensure_removes_a_corrupted_file_even_when_its_download_fails() {
        let content = content();
        let server = serve(|_| Reply::status("503 Service Unavailable")).await;
        let model = model(&server, "/model.onnx", &content);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());
        std::fs::write(store.path(&model), random_bytes(content.len())).unwrap();

        store.ensure(&model).await.unwrap_err();

        assert!(files_in(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn ensure_checks_a_files_hash_only_once_per_store() {
        let content = content();
        let served = content.clone();
        let server = serve(move |_| Reply::ok(&served)).await;
        let model = model(&server, "/model.onnx", &content);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());
        let path = store.ensure(&model).await.unwrap();
        let same_size = random_bytes(content.len());
        std::fs::write(&path, &same_size).unwrap();

        store.ensure(&model).await.unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), same_size);
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ensure_downloads_again_a_model_removed_since() {
        let content = content();
        let served = content.clone();
        let server = serve(move |_| Reply::ok(&served)).await;
        let model = model(&server, "/model.onnx", &content);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());
        let path = store.ensure(&model).await.unwrap();
        std::fs::remove_file(&path).unwrap();

        store.ensure(&model).await.unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), content);
        assert_eq!(server.requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn concurrent_ensures_download_a_model_once() {
        let content = content();
        let served = content.clone();
        let server = serve(move |_| Reply::ok(&served)).await;
        let model = model(&server, "/model.onnx", &content);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());

        let (first, second) = tokio::join!(store.ensure(&model), store.ensure(&model));

        assert_eq!(first.unwrap(), second.unwrap());
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        assert_eq!(files_in(dir.path()), vec![file_name(&model)]);
    }

    #[tokio::test]
    async fn new_builds_no_http_client_until_a_download() {
        let content = content();
        let served = content.clone();
        let server = serve(move |_| Reply::ok(&served)).await;
        let model = model(&server, "/model.onnx", &content);
        let dir = tempfile::tempdir().unwrap();

        let store = ModelStore::new(dir.path().to_path_buf());
        assert!(store.http.get().is_none());
        store.ensure(&model).await.unwrap();

        assert!(store.http.get().is_some());
    }

    #[tokio::test]
    async fn remove_if_present_ignores_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();

        remove_if_present(&dir.path().join("gone")).await.unwrap();
    }

    #[tokio::test]
    async fn remove_if_stale_ignores_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();

        remove_if_stale(&dir.path().join("gone")).await.unwrap();
    }

    #[test]
    fn partial_target_reads_the_file_a_partial_download_becomes() {
        assert_eq!(
            partial_target(".0123456789abcdef-model.onnx.some-uuid.part"),
            Some("0123456789abcdef-model.onnx")
        );
        for name in [
            "0123456789abcdef-model.onnx",
            ".0123456789abcdef-model.onnx",
            "0123456789abcdef-model.onnx.some-uuid.part",
            ".part",
        ] {
            assert_eq!(partial_target(name), None, "{name:?}");
        }
    }

    #[test]
    fn is_revision_of_matches_the_name_after_any_hash_prefix() {
        let model = GLINER_X_SMALL;

        assert!(is_revision_of(&file_name(&model), &model));
        assert!(is_revision_of(
            "0123456789abcdef-gliner-x-small.onnx",
            &model
        ));
        for name in [
            "gliner-x-small.onnx",
            "0123456789ABCDEF-gliner-x-small.onnx",
            "0123456789abcde-gliner-x-small.onnx",
            "0123456789abcdef0-gliner-x-small.onnx",
            "0123456789abcdef-gliner-x-small.tokenizer.json",
            "0123456789abcdef-gliner-x-small.onnx.old",
        ] {
            assert!(!is_revision_of(name, &model), "{name:?}");
        }
    }

    #[test]
    fn path_prefixes_the_name_with_the_hash() {
        let store = ModelStore::new(PathBuf::from("/models"));

        assert_eq!(
            store.path(&GLINER_X_SMALL),
            Path::new("/models/d76b3349fd4bfe44-gliner-x-small.onnx")
        );
    }

    #[test]
    fn registry_entries_are_pinned() {
        let registry = [GLINER_X_SMALL, GLINER_X_SMALL_TOKENIZER];

        for model in registry {
            let revision = model
                .url
                .split("/resolve/")
                .nth(1)
                .and_then(|rest| rest.split('/').next())
                .unwrap_or_default();
            assert_eq!(revision.len(), 40, "{} isn't at a commit", model.url);
            assert!(revision.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(model.url.starts_with("https://"));
            assert_eq!(model.sha256.len(), 64);
            assert!(
                model
                    .sha256
                    .chars()
                    .all(|c| matches!(c, '0'..='9' | 'a'..='f'))
            );
            assert!(model.size > 0);
        }
        let mut names: Vec<_> = registry.iter().map(|model| model.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), registry.len());
    }
}
