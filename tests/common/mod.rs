//! Shared harness for this directory's end-to-end tests: each test starts
//! its own real `fyde-server` (see `fyde_server::spawn_test_server`,
//! backed by a private in-memory database), connects a real `fyde-sdk`
//! [`Client`] to it, and drives a sequence of named, stateful steps
//! against that one long-lived server/client pair — mirroring a real
//! client's lifetime rather than one independent `#[tokio::test]` per
//! assertion.

use std::future::Future;
use std::io::Write as _;

use fyde_sdk::{Client, ClientConfig, LogLevel, Storage};
use lopdf::content::{Content, Operation};
use lopdf::{Object, Stream, dictionary};

/// The password every test account is registered/logged in with. Its
/// value is irrelevant beyond being consistent between a `create` and the
/// matching `login` in the same scenario.
pub const TEST_PASSWORD: &str = "correct horse battery staple";

/// A real `fyde-server`, started fresh for one [`Scenario`], backed by a
/// private in-memory database. Runs on its own dedicated thread for the
/// rest of the process's life (see `fyde_server::spawn_test_server`) —
/// there's no shutdown, since each test gets its own isolated instance
/// rather than sharing one across the binary.
pub struct TestServer {
    url: String,
}

impl TestServer {
    pub fn start() -> Self {
        Self {
            url: fyde_server::spawn_test_server(),
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

/// One long-lived server/client pair, driven through a sequence of named,
/// stateful [`Self::step`]s where later steps depend on state left behind
/// by earlier ones (an uploaded document's id, a logged-in session, ...) —
/// the same way a real client would use the SDK over time, rather than
/// each assertion getting a fresh connection.
///
/// `#[allow(dead_code)]`: see [`wait_for`]'s doc comment — this one skips
/// `reconnect_lifecycle_test.rs`, which drives `Client`/`TestServer`
/// directly since it needs two independently-`init`ed clients.
#[allow(dead_code)]
pub struct Scenario {
    server: TestServer,
    pub client: Client,
}

#[allow(dead_code)]
impl Scenario {
    pub async fn start() -> Self {
        let server = TestServer::start();

        let client = Client::init(ClientConfig {
            url: server.url().to_string(),
            storage: Storage::Memory,
            log_level: LogLevel::Off,
            on_log: None,
            on_document_change: None,
        })
        .await
        .expect("failed to connect the sdk client to the test server");

        Self { server, client }
    }

    /// Runs one named, fallible step, printing its outcome and panicking
    /// with the step's name and error (with full `anyhow` context) if it
    /// fails, so a failure in the middle of a long scenario immediately
    /// says which step broke instead of leaving the reader to guess from a
    /// bare `unwrap` panic.
    pub async fn step<F, Fut>(&self, name: &str, f: F)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<()>>,
    {
        print!("  {name} ... ");
        let _ = std::io::stdout().flush();

        let start = std::time::Instant::now();
        let result = f().await;
        let elapsed = start.elapsed();

        match result {
            Ok(()) => println!("\u{2713} ({elapsed:.2?})"),
            Err(err) => {
                println!("\u{2717} ({elapsed:.2?})");
                panic!("step `{name}` failed after {elapsed:.2?}: {err:#}");
            }
        }
    }
}

/// Polls `predicate` every 50ms until it returns `Some`, or panics once
/// `timeout` elapses. Used to wait for an effect of the changelog sync job
/// `UsersService::create`/`login` start automatically (or any other
/// eventually-consistent state), which has no other completion signal.
///
/// `#[allow(dead_code)]`: `tests/common/mod.rs` is compiled fresh into each
/// test binary in this directory, so an item only some of them use (this
/// one skips `session_lifecycle_test.rs`, which never waits on sync) is
/// flagged dead in that binary's copy.
#[allow(dead_code)]
pub async fn wait_for<T, F, Fut>(timeout: std::time::Duration, mut predicate: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(value) = predicate().await {
            return value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition not met within {timeout:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// A username unique enough to never collide with another scenario's
/// account, even though every scenario gets its own isolated server.
pub fn random_username() -> String {
    format!("e2e-test-user-{}", uuid::Uuid::new_v4())
}

/// Builds a minimal, single-page PDF containing `text`, for tests that need
/// real PDF bytes to upload (`Service::upload` only accepts `.pdf` files)
/// and a transcript to extract. Mirrors `fyde-sdk`'s own
/// `transcript::tests::build_pdf` fixture, which is private to this crate.
pub fn build_pdf(text: &str) -> Vec<u8> {
    let mut doc = lopdf::Document::with_version("1.5");

    let pages_id = doc.new_object_id();

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! {
            "F1" => font_id,
        },
    });

    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 48.into()]),
            Operation::new("Td", vec![100.into(), 600.into()]),
            Operation::new("Tj", vec![Object::string_literal(text)]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));

    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });

    let pages = dictionary! {
        "Type" => "Pages",
        "Kids" => vec![page_id.into()],
        "Count" => 1,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages));

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

/// Writes a real PDF containing `text` to a temporary file, for tests that
/// need a path to pass to `Service::upload`.
pub fn write_temp_pdf(text: &str) -> tempfile::NamedTempFile {
    let mut file = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
    file.write_all(&build_pdf(text)).unwrap();
    file
}
