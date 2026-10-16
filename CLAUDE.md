# CLAUDE.md

`fyde-sdk` is a Rust client library (no binary) for a fyde server (`../server`). It's built
standalone (not a Cargo workspace member); `../scripts`, `../cli` and `../application` (via
UniFFI, `src/ffi.rs`) consume it. Protos come from the Buf Schema Registry
(`buf.build/fyde-lab/api`, published from `../server/protos`).

## Commands

```sh
cargo build
cargo test                       # or: cargo test <name>
cargo clippy --all-targets -- -D warnings   # what CI runs (with cargo fmt --check, cargo test)
cargo fmt
FYDE_PROTO_DIR=../server/protos cargo build  # build without buf / network access
```

- Dev shell: `shell.nix` via direnv. Rust **1.98+** (`wreq`'s MSRV); `protobuf`; `cmake`/`perl`/
  `go`/`clang` for `wreq`'s vendored BoringSSL; `webkitgtk_4_1`/`gtk3` for `wry`.
- `build.rs` runs `buf export buf.build/fyde-lab/api` (needs `buf` on `PATH` + network) unless
  `FYDE_PROTO_DIR` is set.
- `tests/` (and so `cargo test` / `clippy --all-targets`) need `../server` checked out
  (`fyde-server` is a path dev-dependency).

## Non-negotiable rules

**Layering.** All business logic (validation, orchestration, cursor/offset bookkeeping, rules)
lives in each domain's `service.rs`. `grpc_client.rs` only sends protos and returns raw
responses; `crypto.rs` only encrypts/decrypts; storage adapters (`storage_sqlite.rs`,
`SqliteClient`) only read/write rows. Move any decision found elsewhere into `service.rs`.

**Errors** (`src/error/`):
- Every fallible function returns the crate's `Result<T>`. Never add per-module error types.
- Wrap every propagated error with context: `.context("failed to open local database")` /
  `.with_context(|| format!(...))` (`ErrorContext`). No bare `?` without context.
- Raise failures as `ErrorKind::X(..).into()`. New failure: add an `ErrorKind` variant, map it
  in `ErrorKind::code`, and if it needs its own code add it to `for_each_error_code!` (generates
  both `ErrorCode` and `ffi::FfiError`) plus wording in every language in `messages.rs`.
- `ErrorCode` values (`1xxx` device, `2xxx` server, `3xxx` account, `4xxx` documents, `5xxx`
  scripts) are persisted by callers: **never renumber or reuse one**.
- FFI: every exported `FydeClient` method runs through `FydeClient::localized`; never add a
  `From<Error> for FfiError`.

**Wire formats — never break old events or older clients:**
- `Attribute` variants (`documents/attributes/models.rs`, serialized via `AttributeWire`):
  never change or remove one; add one only deliberately (older clients fail to decode the
  whole metadata).
- `Metadata` serializes positionally (MessagePack) through `MetadataWire`, which keeps empty
  slots for removed fields: never remove a field outright; new fields need `#[serde(default)]`.

**Security invariants:**
- Every authenticated gRPC call goes through `sessions::authenticated_request` **and** passes
  its result through `self.sessions.check_response(...)` (except registration/login steps and
  `logout`). That's what turns a rejected token into `ErrorCode::SessionExpired` + local wipe.
- A new secret must be added to `secrets::ALL_SECRETS`, or logout leaves it behind. Secrets
  (master key, session token) never go in the `settings` table.
- `logout` always clears secrets and wipes the database, whether or not the server answers.
- Every Lua VM is built with `tools::sandbox::new_vm` and scripts loaded with
  `sandbox::load_source` — never `Lua::new*` / `lua.load` (that would run precompiled bytecode).
- Scraper debug reports never record request headers, request/download bodies or form values;
  response bodies only with `debug_http_dump`.
- Never write to the `documents_fts` FTS5 table directly (triggers maintain it). A new virtual
  table must support `'delete-all'` so `SqliteClient::wipe` can clear it.

## Testing rules

- **`service.rs` tests use only `mockall` mocks** of injected traits, never a real
  `grpc_client.rs`, `crypto.rs` or SQLite storage. Mark dependency traits `#[async_trait]` +
  `#[cfg_attr(test, mockall::automock)]` (convert `-> impl Future` signatures to `async fn`).
  Configure each mock inside its test with exactly the expected calls (an unexpected call
  panicking is often the assertion) — no shared fake-storage builders. A small inline
  `Arc<Mutex<_>>` capture is fine when a test must read back a generated value.
- Exceptions, which test real adapters: `grpc_client.rs`, `crypto.rs`, `tools/sql/sqlite.rs`,
  `storage_sqlite.rs` files, `secrets/storage_keystore.rs` (keyring's in-memory mock),
  `changelog/storage_settings.rs`. `changelog::Service` can't be automocked (its
  `start_consume_job` takes a boxed `FnMut`), so use the hand-written `RecordingChangelog`-style
  fakes for it.
- Fixtures: entity structs returned by a domain have a `#[cfg(test)]` `Fake*` builder in the
  same `models.rs` (`FakeDocument::new().with_x(..).build()`; see `FakeDocument`,
  `FakeMetadata`, `FakeChangelogEvent`, `FakeScript`, ...), using `tools::testing`'s random
  helpers. Use them instead of struct literals. Keep them `pub(crate)` and re-export one only
  when another file needs it (unused re-exports fail clippy).
- Every top-level test in `tests/*_test.rs` must be `#[serial(e2e)]` (`serial_test`): each
  spins up a real server and real OPAQUE/Argon2id, and parallel runs make `tests/common`'s
  timeouts flaky. `tests/` suites using `Storage::Disk` must call
  `use_in_process_keystore_for_tests` so they never touch the real keychain.

## Architecture

`Client::init(ClientConfig)` (`src/lib.rs`) opens a lazy gRPC channel to `config.url`
(rustls + bundled webpki roots for `https://`), opens the SQLite database selected by
`config.storage` (`Storage::Disk(dir)` → `<dir>/<client_name>/fyde.db`, `Storage::Memory`) and
wires every domain. `client_name` must be a safe single path segment (`InvalidClientName`).
Public accessors: `documents()`, `attributes()`, `users()`, `settings()`, `server_state()`,
`scripts()`, `scrapers()`, `usage()`, `user_message(&err)`. `ClientConfig` carries the FFI
callbacks (`on_log`, `on_document_change`, `on_scraper_progress`, `on_scraper_question`).

New server services follow the same pattern: a module under `src/domains/` with `mod.rs`
(the `Service` trait, methods take `&self` so it can live behind `Arc<dyn Service>`),
`models.rs` (plain structs), `service.rs`, and adapters; wired into `Client::init`.
Non-domain helpers live in `src/tools/` (`log`, `sandbox`, `sql`, `testing`).

### Domains (`src/domains/`)

- **documents** — no gRPC transport: every write is a `changelog` event. `upload` needs a
  master key, rejects a known checksum (`DuplicateDocument`, before parsing), parses, then
  publishes an encrypted `SaveDocument` event; the document reaches the local cache only when
  the sync job consumes it back. `get`/`list`/`search`/`search_facets` are local-only
  (`storage_sqlite.rs`, FTS5 `bm25` ranking over contentless `documents_fts`).
  `search_facets` counts each field with its own filter lifted so UIs only offer reachable
  choices. A `Subcategory` belongs to one `Category`; mismatches fail
  (`MismatchedSubcategory`).
  - `attributes/` — typed per-document values (`Metadata::attributes`, name → `Attribute`;
    amounts are `i64` minor units, never floats) and their server-side *definitions*
    (`AttributesService::list`/`set`/`check`). Checks (`check_values`, then defined types) run on
    upload and `update_metadata`. Promote anything that needs filtering/sorting to a real
    `Metadata` field instead of querying attributes JSON.
  - `parser/` — `ParserService` (`init_parser`): derives metadata, extracts text (PDF/office),
    runs the user's enabled classification scripts in a sandboxed VM.
  - `dev.rs` — `DevDocumentsClient`, plain-file writes for `init_dev_scrapers`.
- **changelog** (crate-private) — the server is a blind relay of client-encrypted events
  (`SaveDocument`, `UpdateMetadata`, `InstallScript`). `crypto.rs`: AES-256-GCM, per-event DEK
  wrapped by a KEK derived from the master key. The consume job replays from the persisted
  cursor then tails live, materializing documents and installed scripts; only the process
  holding the `flock` (`write_lock`) writes local storage.
- **users** — OPAQUE (RFC 9807, `opaque-ke`); the password never leaves the SDK.
  `FydeCipherSuite` (ristretto255 + 3DH/SHA-512 + Argon2id 64 MiB/3 iters/4 lanes) must match
  `../server`'s. `crypto.rs`'s `OpaqueClient` trait exists so service tests skip real KSF cost.
  The master-key wrapping key is HKDF'd from the OPAQUE export key. A wrong password usually
  fails locally (`InvalidCredentials`). One account per local database (`AlreadyLoggedIn`).
  `login` persists the session token last. `login` returns `LoginOutcome::LoggedIn` or
  `SessionLimitReached { sessions, .. }`; the caller re-runs the whole login with
  `replace_session_id` (the server's `login_id` is single-use). `list_sessions`/`revoke_session`
  manage open sessions (never cached).
- **secrets** (crate-private) — master key and session token in the OS credential store
  (`keyring-core` + per-platform stores; only Linux/Secret Service has been exercised), entries
  namespaced per database path. If no store is usable, `create`/`login` fail with
  `SecureStorageUnavailable` unless called with `allow_insecure_storage`, which writes to the
  `insecure_secrets` table. Android apps must call `io.crates.keyring.Keyring`'s
  `initializeNdkContext(context)` before the first `Client::init`.
- **sessions** (crate-private) — `authenticated_request`, `check_response` (see rules above;
  the `ExpiredSessionHandler` wipe runs on its own spawned task), `is_connected`.
- **scripts** — server-side script CRUD and per-user enabling; `install_script` publishes an
  encrypted `InstallScript` event, and installed scripts are read from the local
  `installed_scripts` table. `InMemoryScriptStorage` is a server-less `Service` for tooling.
- **settings** (public key/value, non-secret), **server_state** (gRPC health; never fails),
  **usage** (`get_usage`, plan limits + current usage; never cached).
- **scrapers** — runs Lua scripts that log into third-party sites. Contract: the script
  returns `{ run = function(parameters) ... }` and only calls the `fyde` table built in
  `host/` (`http`, `html`, `json`, `log`, `progress`, `input`, `save_document`, `session`,
  `browser`).
  - `mlua::Lua` is `!Send`: `run` builds the VM inside `spawn_blocking` and host functions use
    a captured runtime `Handle::block_on`. Never move a `Lua` across threads.
  - `save_document` uploads via `documents::Service::upload`; `DuplicateDocument` is skipped.
  - `session/`, `cookies/`, `reports/` persist per scraper through a `StorageConfig`
    (`Sqlite` for a `Client`, `File` for `init_dev_scrapers`). `fyde.session` is saved even
    on failure; cookies only on success (a failed run deletes them).
  - `allowed_domains` fails closed: an empty list allows no host; an IP literal only matches
    exactly.
  - `browser/` — `wry`/`tao` webview, started lazily on a dedicated thread (all its objects are
    `!Send`; calls cross over via `EventLoopProxy` commands and IPC replies). Only verified on
    Linux/WebKitGTK. Webview cookies are merged into the saved jar (browser wins) and restored
    as domain cookies (`driver::to_webview_cookie`). Forms are submitted with `click`, never
    an in-page `fetch` (it bypasses bot-mitigation hooks); `download` captures native
    downloads.
  - `run_installed` runs an installed script (`wreq_emulation`/`follow_redirects` default on).
  - `crate::init_dev_scrapers` builds a scrapers service without a `Client` (file storage +
    `DevDocumentsClient`); `../scripts` is its caller.

### Local database (`src/tools/sql/sqlite.rs`, `migrations/`)

Single-connection pool (one SQLite writer). In-memory databases use a uniquely named
shared-cache DB plus a keeper connection, so sqlx replacing a pooled connection never yields an
empty, unmigrated DB. Files are `0600`, `PRAGMA secure_delete = ON`, and `wipe` ends with
`VACUUM`; it skips FTS5 shadow tables and clears virtual tables last with `'delete-all'`.
