# fyde-sdk

Rust client library for a fyde server. It handles accounts (OPAQUE password authentication), end-to-end encrypted documents synced through the server's changelog, and Lua scripts: parsers that classify documents and scrapers that fetch them from third-party sites. It is a plain Rust library: the Kotlin Multiplatform app (`../application`) reaches it through its own native bindings crate (`application/sdkBridge/rust`).

```rust
use fyde_sdk::{Client, ClientConfig, LogLevel, Storage};

let client = Client::init(ClientConfig {
    url: "http://127.0.0.1:40051".into(),
    client_name: "desktop".into(),
    // The database lands at fyde-data/desktop/fyde.db.
    storage: Storage::Disk("fyde-data".into()),
    log_level: LogLevel::Info,
    on_log: None,
    on_document_change: None,
    on_scraper_progress: None,
    on_scraper_question: None,
})
.await?;

client.users().login("alice", "correct horse", "laptop").await?;
let documents = client.documents().list(0, 20).await?;
```

## Building

Requirements:

- Rust 1.98 or later.
- `protoc`.
- On Linux, the WebKitGTK/GTK/D-Bus development packages used by the scrapers' embedded browser and the Secret Service keyring.

`../shell.nix` provisions all of these.

The gRPC protos come from the Buf Schema Registry module `buf.build/fyde-lab/api`, so a build needs either:

- `buf` on `PATH` with network access to `buf.build`, or
- a local checkout of the protos (`protos/` in [`server`](https://github.com/fyde-lab/server)), pointed to by `FYDE_PROTO_DIR`.

```sh
cargo build                                  # protos from the BSR
FYDE_PROTO_DIR=../server/protos cargo build  # protos from a local checkout
```

## Testing

```sh
cargo test       # unit tests
cargo clippy --all-targets
cargo fmt --check
```

The end-to-end suites, which drive a real `Client` against a real server, live in the [`server`](https://github.com/fyde-lab/server) repository's `tests/`.

## Further reading

`CLAUDE.md` describes the architecture domain by domain, along with the conventions for layering, testing and errors.
