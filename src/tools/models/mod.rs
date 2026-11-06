//! The on-device model files the SDK can run (see `tools::onnx`), each pinned
//! to a fixed upstream revision, size and SHA-256, and [`ModelStore`], which
//! downloads them into `ClientConfig::models_dir`.
//!
//! That directory is never the client's own one: logout deletes every file
//! there, and models aren't user data. Nothing downloads on `Client::init`:
//! only [`ModelStore::ensure`] does, when a model is first needed.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use futures::StreamExt as _;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt as _;

use crate::error::{ErrorContext, ErrorKind, Result};

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

macro_rules! multilingual_e5_small_url {
    ($file:literal) => {
        concat!(
            "https://huggingface.co/Xenova/multilingual-e5-small/resolve/",
            "761b726dd34fb83930e26aab4e9ac3899aa1fa78/",
            $file
        )
    };
}

/// multilingual-e5-small (MIT), int8-quantized: 384-dimension text
/// embeddings, inputs up to 512 tokens, prefixed `query: ` or `passage: `.
pub(crate) const MULTILINGUAL_E5_SMALL: Model = Model {
    name: "multilingual-e5-small.onnx",
    url: multilingual_e5_small_url!("onnx/model_quantized.onnx"),
    size: 118_308_185,
    sha256: "f80102d3f2a1229f387d3c81909990d8945513e347b0eab049f7de3c6f98c193",
};

/// [`MULTILINGUAL_E5_SMALL`]'s tokenizer.
pub(crate) const MULTILINGUAL_E5_SMALL_TOKENIZER: Model = Model {
    name: "multilingual-e5-small.tokenizer.json",
    url: multilingual_e5_small_url!("tokenizer.json"),
    size: 17_082_730,
    sha256: "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a download may go without receiving a byte before it fails.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// A partial download untouched for this long was abandoned (its process
/// died before cleaning it up): an active one writes at least every
/// [`READ_TIMEOUT`].
const STALE_PARTIAL_AGE: Duration = Duration::from_secs(10 * 60);
const PARTIAL_SUFFIX: &str = ".part";

/// Downloads [`Model`]s into a directory, verified and atomically.
pub(crate) struct ModelStore {
    dir: PathBuf,
    http: wreq::Client,
}

impl ModelStore {
    /// Doesn't touch `dir`: [`ModelStore::ensure`] creates it when needed.
    pub(crate) fn new(dir: PathBuf) -> Result<Self> {
        let http = wreq::Client::builder()
            // Hugging Face redirects every download to its CDN.
            .redirect(wreq::redirect::Policy::default())
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .map_err(ErrorKind::ModelDownload)
            .context("failed to build the model download HTTP client")?;

        Ok(Self { dir, http })
    }

    /// Where `model` lives once downloaded. The SHA-256 prefix keeps a
    /// re-pinned model from ever being mistaken for its previous revision.
    pub(crate) fn path(&self, model: &Model) -> PathBuf {
        self.dir.join(file_name(model))
    }

    /// Returns `model`'s local path, downloading it first unless a file of
    /// its size is already there. A download goes to a temporary file in
    /// the same directory, is checked against the pinned size and SHA-256
    /// (failing with `ModelCorrupted` otherwise), then renamed into place:
    /// a failed, mismatched or cancelled download never leaves a file at
    /// that path, and removes its temporary one.
    pub(crate) async fn ensure(&self, model: &Model) -> Result<PathBuf> {
        let path = self.path(model);
        if let Ok(metadata) = tokio::fs::metadata(&path).await
            && metadata.len() == model.size
        {
            return Ok(path);
        }

        tokio::fs::create_dir_all(&self.dir)
            .await
            .with_context(|| {
                format!(
                    "failed to create the models directory {}",
                    self.dir.display()
                )
            })?;
        self.remove_stale_partials(model)
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

        Ok(path)
    }

    async fn download(&self, model: &Model, to: &Path) -> Result<()> {
        let response = self
            .http
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
        let sha256 = hex(&hasher.finalize());
        if sha256 != model.sha256 {
            return Err(ErrorKind::ModelCorrupted(format!(
                "SHA-256 {sha256} instead of {}",
                model.sha256
            ))
            .into());
        }

        Ok(())
    }

    /// Removes `model`'s partial downloads nothing has written to for
    /// [`STALE_PARTIAL_AGE`], keeping those another download (in this
    /// process or another sharing the directory) may still be writing.
    async fn remove_stale_partials(&self, model: &Model) -> Result<()> {
        let prefix = format!(".{}.", file_name(model));
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
            if !name.starts_with(&prefix) || !name.ends_with(PARTIAL_SUFFIX) {
                continue;
            }
            let modified = entry
                .metadata()
                .await
                .and_then(|metadata| metadata.modified())
                .with_context(|| format!("failed to read the age of {name}"))?;
            let age = SystemTime::now()
                .duration_since(modified)
                .unwrap_or_default();
            if age >= STALE_PARTIAL_AGE {
                tokio::fs::remove_file(entry.path())
                    .await
                    .with_context(|| format!("failed to remove {name}"))?;
            }
        }

        Ok(())
    }
}

fn file_name(model: &Model) -> String {
    format!("{}-{}", &model.sha256[..16], model.name)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
            sha256: leak(hex(&Sha256::digest(content))),
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
        let store = ModelStore::new(dir.path().to_path_buf()).unwrap();

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
        let store = ModelStore::new(dir.path().to_path_buf()).unwrap();

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
        let store = ModelStore::new(dir.clone()).unwrap();

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
        let store = ModelStore::new(dir.path().to_path_buf()).unwrap();

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
        let store = ModelStore::new(dir.path().to_path_buf()).unwrap();
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
        let store = ModelStore::new(dir.path().to_path_buf()).unwrap();

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
        let store = ModelStore::new(dir.path().to_path_buf()).unwrap();

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
        let store = ModelStore::new(dir.path().to_path_buf()).unwrap();

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
        let store = ModelStore::new(dir.path().to_path_buf()).unwrap();

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
        let store = Arc::new(ModelStore::new(dir.path().to_path_buf()).unwrap());

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
    async fn ensure_fails_on_an_http_error_and_leaves_no_file() {
        let server = serve(|_| Reply::status("404 Not Found")).await;
        let model = model(&server, "/model.onnx", &content());
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf()).unwrap();

        let err = store.ensure(&model).await.unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelDownloadFailed);
        assert!(err.has_context());
        assert!(err.to_string().contains("404"), "{err}");
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
        let store = ModelStore::new(dir.path().to_path_buf()).unwrap();

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
        let other = Model {
            name: "other.onnx",
            ..model
        };
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf()).unwrap();
        let partial =
            |model: &Model, id: &str| format!(".{}.{id}{PARTIAL_SUFFIX}", file_name(model));
        let long_ago = SystemTime::now() - STALE_PARTIAL_AGE - Duration::from_secs(1);
        for (name, modified) in [
            (partial(&model, "stale"), Some(long_ago)),
            (partial(&model, "active"), None),
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
            partial(&other, "stale"),
        ];
        expected.sort();
        assert_eq!(files_in(dir.path()), expected);
    }

    #[test]
    fn path_prefixes_the_name_with_the_hash() {
        let store = ModelStore::new(PathBuf::from("/models")).unwrap();

        assert_eq!(
            store.path(&MULTILINGUAL_E5_SMALL),
            Path::new("/models/f80102d3f2a1229f-multilingual-e5-small.onnx")
        );
    }

    #[test]
    fn registry_entries_are_pinned() {
        let registry = [MULTILINGUAL_E5_SMALL, MULTILINGUAL_E5_SMALL_TOKENIZER];

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
