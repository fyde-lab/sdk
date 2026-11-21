# CLAUDE.md

`fyde-sdk`: the Rust client library (no binary) for `../server`, built standalone, in a workspace
with `browser-wry/` (`fyde-browser-wry`, the desktop webview engine `fyde.browser` runs on, kept
out of `fyde-sdk` so mobile builds never pull in tao/wry/GTK). Consumed by
`../scripts`, `../cli` and `../application` through its bindings crate
`../application/sdkBridge/rust`, which mirrors this crate's public API: **update it with any API
change**. Protos come from the BSR (`buf.build/fyde-lab/api`, published from `../server/protos`).
Shared layering and error conventions are in `../CLAUDE.md`. Each domain's `mod.rs` (`//!` doc +
`Service` trait) is the contract to read before touching it.

## Commands

```sh
cargo build --workspace
cargo test --workspace                       # or: cargo test <name>
cargo clippy --workspace --all-targets -- -D warnings  # CI: this + cargo fmt --all --check + cargo test --workspace
xvfb-run -a cargo test -p fyde-browser-wry -- --ignored  # the real wry webview (needs a display)
FYDE_PROTO_DIR=../server/protos cargo build  # without buf / network
RUSTFLAGS="--cfg tokio_unstable" cargo build --features tokio-console  # tokio-console on 127.0.0.1:6669
FYDE_TEST_MODELS_DIR=/some/dir cargo test -- --ignored multilingual_e5  # real-model tests (also: gliner_x_small, pp_ocrv5)
```

- Dev shell: `shell.nix` via direnv. Rust **1.95+** (`oar-ocr`'s MSRV), `protobuf`, `dbus` (the
  Secret Service keyring), and the `webkitgtk`/`gtk3` that `fyde-browser-wry`'s `wry` needs (not
  `fyde-sdk` itself). No `cmake`/`bindgen`.
- `build.rs` runs `buf export` (needs `buf` + network) unless `FYDE_PROTO_DIR` is set. On
  static-ONNX targets, `ort-sys` downloads pyke's library on the first build (`ORT_LIB_PATH` to
  use a local one).

## Non-negotiable rules

**Layering.** Business logic (validation, orchestration, cursor/offset bookkeeping, rules) lives
only in each domain's `service.rs`. `grpc_client.rs` only sends protos, `crypto.rs` only
encrypts/decrypts, storage adapters (`storage_sqlite.rs`, `SqliteClient`) only read and write rows.

**Errors** (`src/error/`):
- Every fallible function returns the crate's `Result<T>`. Never add per-module error types.
- Wrap every propagated error with `.context(..)` / `.with_context(|| ..)`. No bare `?`.
- Raise errors as `ErrorKind::X(..).into()`. To add a new failure: add an `ErrorKind` variant, map
  it in `ErrorKind::code`, and if it needs its own code, add it to `for_each_error_code!` (also
  exported to the app's bindings) with wording in every language in `messages.rs`.
- `ErrorCode`s (`1xxx` device, `2xxx` server, `3xxx` account, `4xxx` documents, `5xxx` scripts)
  are persisted by callers: **never renumber or reuse one**.

**Wire formats: never break old events or older clients.**
- `Attribute` variants (`AttributeWire`): never change or remove one. Add one only deliberately,
  because older clients then fail to decode the whole metadata.
- `Metadata` is positional MessagePack (`MetadataWire`, which keeps slots for removed fields):
  never remove a field, and give new fields `#[serde(default)]`.
- Changelog event payloads must keep decoding forever, e.g. never add `deny_unknown_fields` to
  `Folder` (old `SaveFolder` events carry a `description`).

**Security invariants:**
- Every authenticated gRPC call goes through `sessions::authenticated_request` **and**
  `self.sessions.check_response(..)` (except registration/login steps and `logout`). That's what
  turns a rejected token into `SessionExpired` + a local wipe.
- A new secret goes in `secrets::ALL_SECRETS`, or logout leaves it behind. Secrets never go in
  the `settings` table.
- Key material (master key, DEKs, KEKs, OPAQUE export keys, recovery keys and entropy) and every
  value read from `secrets` (its cache included) are held in `zeroize::Zeroizing`. A type
  carrying a secret (session token, script parameters) implements `Debug` by hand and redacts
  it. Never derive `Debug` for one.
- `logout` always clears secrets, wipes the DB and deletes every other file in the client
  directory (`tools::client_dir`), whether or not the server answers. Everything a domain writes
  to disk must live there. Plaintext-derived memory (the documents `EmbeddingCache`) is cleared by
  `users`' `clear_local_state`, after the secrets. Never log embedding vectors or document text.
- Lua VMs are built only with `tools::sandbox::new_vm` and scripts loaded only with
  `sandbox::load_source`. Never use `Lua::new*` / `lua.load`, which would run precompiled bytecode.
- Scraper reports never record request headers, request/download bodies, form values or
  `fyde.input.ask` answers. Response bodies are recorded only with `debug_http_dump`. Every logged
  or reported URL goes through `reports::redact_url`, and `reqwest` errors are recorded
  `without_url()`.
- `allowed_domains` fails closed: an empty list allows no host, and an IP literal only matches
  exactly. Never add process-wide mutable state a script can reach.
- Model files live in `ClientConfig::models_dir`, never the client directory (`init` rejects one
  inside it). They land there only through `ModelStore::ensure`, which checks size + SHA-256 and
  then renames the file into place. Pin new models to a fixed revision, never a branch.
  `Client::init` never waits on a download: `documents::start_model_loading` loads the embedding
  and OCR models in the background. GLiNER is never preloaded: a scraper run's setup or the first
  upload loads it, and it's unloaded once idle (`gliner.rs`). Unit tests that `init` a `Client`
  pass a regular file as `models_dir`.
- Every `reqwest` client starts from `tools::http::client_builder` (rustls on `ring`). Keep
  `rustls`' default features off everywhere, or `aws-lc-rs` and its slow C build come back.
  Under `browser_emulation`, the default headers are `uagent`'s captured Firefox headers (pinned
  exactly). The SDK writes no header values itself.
- Never write to `documents_fts` (triggers maintain it). A new virtual table must support
  `'delete-all'` so `SqliteClient::wipe` can clear it.

**Never-skip / coupled changes:**
- A changelog event that keeps failing is never skipped (`POISON_PILL_PASSES` only escalates the
  log level). **Ask before adding any skip mechanism.**
- Bump `CHUNKER_VERSION` whenever `chunk.rs` or the passage format changes.
- Changing how thumbnails are drawn needs a migration resetting `documents.thumbnail` to NULL.
- `DEFAULT_SIMILAR_MIN_CONFIDENCE` maps back to `SIMILARITY_FLOOR` (a test checks). Move them
  together.
- `users`' `FydeCipherSuite` must match `../server`'s.
- `scrapers`: a new `ProgressStep` needs wording in every language plus a variant in the
  bindings. `host/helpers.lua`'s names and behaviour are relied on by `../scripts`.
- `documents::gliner`: keep the model's details exactly as they are (special token ids, widths,
  windows, `split_words`). `documents::extraction`: keep `doc-ai-poc`'s labels, decoys,
  threshold, word limit and parsers unless they're re-tuned.

## Testing rules

- **`service.rs` tests use only `mockall` mocks** of injected traits, never a real
  `grpc_client.rs`, `crypto.rs` or SQLite storage. Dependency traits are `#[async_trait]` +
  `#[cfg_attr(test, mockall::automock)]` (use `async fn`, not `-> impl Future`). Configure each
  mock in its test with exactly the expected calls. Shared fake-storage builders aren't allowed;
  an inline `Arc<Mutex<_>>` capture is.
- Adapters test themselves for real: `grpc_client.rs`, `crypto.rs`, `storage_sqlite.rs`,
  `tools/sql/sqlite.rs`, `tools/client_dir.rs`, `tools/models` (a local HTTP server),
  `tools/onnx`, `documents/{embedder,gliner}.rs`, `documents/transcript/` (PDFs built with
  `lopdf`), `secrets/storage_keystore.rs`, `changelog/storage_settings.rs`,
  `browser-wry/` (needs a display: `xvfb-run`). Real-model and real-webview
  tests are `#[ignore]`. `browser/driver.rs` tests against mocked engines (its engine traits are
  sync: an engine answers from its UI thread). `changelog::Service` can't be automocked (its
  `start_consume_job` takes a boxed `FnMut`), so use `RecordingChangelog`-style fakes.
- Fixtures: every entity a domain returns has a `#[cfg(test)]` `Fake*` builder in its
  `models.rs` (`FakeDocument::new().with_x(..).build()`) using `tools::testing`. Use those builders,
  not struct literals. Keep the builders `pub(crate)`, and re-export one only where another file
  needs it, since unused re-exports fail clippy. Builders other crates need are
  `#[cfg(any(test, feature = "testing"))]`, `pub`, and re-exported from `fyde_sdk::testing`.
- No `tests/` here: end-to-end suites live in `../server/tests/` (which dev-depends on this
  crate). Keep what they use public (e.g. `#[doc(hidden)] use_in_process_keystore_for_tests`) and
  update them with any API change.

## Architecture

`Client::init(ClientConfig)` (`src/lib.rs`) opens a lazy gRPC channel (rustls + webpki roots),
opens the SQLite DB (`Storage::Disk(dir)` → `<dir>/<client_name>/fyde.db`, or `Storage::Memory`),
wires every domain and starts the background jobs (changelog consumer, thumbnails, model
loading, scraper report uploads; each but the uploads can be turned off in
`ClientConfig::background_jobs`, which only defers its work, never disables a feature, except
`thumbnails`: `DocumentsService::thumbnail` only reads what that job stored, so off there are none), which
are aborted when the `Client` drops. A disk DB is guarded by an exclusive `flock`
(`ClientAlreadyRunning`), so every domain may write local storage freely. `ClientConfig` also
carries the caller's callbacks (`on_log`, `on_document_change`, `on_scraper_*`).

A new domain is a module under `src/domains/` with `mod.rs` (the `Service` trait, `&self`
methods, used behind `Arc<dyn Service>`), `models.rs`, `service.rs` and adapters, wired into
`Client::init`. Non-domain helpers are in `src/tools/`.

### Domains (`src/domains/`)

- **documents**: no transport. Writes are changelog events, reads are the local cache (FTS5 +
  embeddings, hybrid search). Sub-modules: `attributes/` (typed values, `i64` minor units for
  amounts, never floats; anything needing filtering/sorting becomes a real `Metadata` field;
  `retain_valid` drops what `check` would refuse, for extracted values), `transcript/` (PDF text
  layer via `hayro`, OCR via `oar-ocr` for scanned pages; OCR never fails an upload),
  `classification/` (zero-shot type + content-based folder guess), `extraction/` (attribute
  values by `DocumentType`, found by GLiNER; never fails an upload), `embedder.rs`, `gliner.rs`
  (the model is held via `ModelHold`, then unloaded after `attribute_model_idle_timeout`),
  `thumbnail.rs` (drawn only by the background job; `thumbnail(id)` never draws). Transcription, embedding, classification and extraction run at upload, never
  on consumption. The transcript and attributes travel in the event; embeddings never do.
- **changelog** (crate-private): blind relay of encrypted events. The consume job materializes
  every other domain's state, checking the `previous_id` chain and recording applied events.
- **folders**: validated locally, published as events, applied by a deterministic conflict
  resolver.
- **users**: OPAQUE (`opaque-ke`). Argon2id `finish_*` steps run on `spawn_blocking`. Includes
  sessions and BIP39 recovery keys.
- **secrets** (crate-private): OS credential store (`keyring-core`, only Linux tested), with an
  `insecure_secrets` table fallback only when `allow_insecure_storage` is passed. Android apps
  must call `Keyring.initializeNdkContext(context)` before `Client::init` (`system_language`
  reads the device locale over JNI through it too: Android has no `LANG`).
- **sessions** (crate-private): `authenticated_request`, `check_response`, `is_connected`.
- **scripts**: reads scripts from `scripts.v1` and enables/disables them per script on the
  server, where the plan quota is also enforced. Writing scripts and icons is admin-only and not
  in this SDK. A script can be installed several times (one *installation* per site account,
  each with its own id, label and optional `folder_id`), published as
  `InstallScript`/`UninstallScript` events. A string `ScriptParameter` may carry `choices`, and
  `install_script` refuses a value outside them (`InvalidScriptParameterChoice` 5013). Callers
  enable a script on install and disable it once its last installation is gone.
- **scrapers**: Lua runs. `mlua::Lua` is `!Send`, so a VM is built inside `spawn_blocking` and
  never moves threads. Blocking host calls use `StopSignal::block_on`, never a bare
  `Handle::block_on`. `fyde.http` is `reqwest` with a `cookie_store` jar, and redirects are
  checked hop by hop against `allowed_domains`. Reports' rules live in `reports/service.rs`.
  `browser/`: `driver.rs` holds every rule (allowed navigations, popups and downloads, the JS,
  the cookies kept); a `WebviewEngine` (`engine.rs`, public as `fyde_sdk::browser`, whose module
  doc is the contract) only hosts webviews: one isolated, ephemeral data store per session,
  decisions asked of the driver's `WebviewHandler`. Callers pass one as
  `ClientConfig::browser_engine` / `init_dev_scrapers`'s `browser_engine` (`None`: `fyde.browser`
  fails with `BrowserFailed`): `fyde_browser_wry::WryEngine` on desktop (every `wry` webview on
  one process-wide thread, for GTK), the app's native webviews on Android/iOS. Never put a rule
  in an engine. Engines raise errors with `fyde_sdk::Error::browser`.
  Forms are submitted with `click`, never an in-page `fetch`, and downloads never touch the disk.
  A finished run's report of a known script (an installation's) is uploaded once to the server's
  `reports.v1`, which links it to no user, by a background job (`reports::start_upload_job`),
  with an id derived from its run's (`service::run_id`, a hash: the v7 id would tell when the
  run started), which the server stores once,
  without the run's name (scrubbed from its log too) or start time
  (`reports::service::report_json`, at most 1 MiB).
  `crate::init_dev_scrapers` (for `../scripts`) runs scrapers without a `Client`.
- **icons** (read-through cache, never revalidated), **settings** (callers' keys are prefixed
  `app.`), **server_state** (never fails), **usage** (never cached).

### On-device models and ONNX Runtime

`tools::models` is the pinned registry and `ModelStore` (see its module doc). `tools::onnx` loads
sessions and tokenizers. `ort` is pinned `=2.0.0-rc.13`, the version `oar-ocr` (pinned, with
`default-features = false`) uses: **bump both together**. Linking is per target, decided by the
two target-specific `ort` entries in `Cargo.toml` and `build.rs`'s `ort_links_statically`.
**Keep all three in sync.**

- **Static** (pyke ONNX Runtime 1.28): Linux gnu x86_64/arm64, macOS arm64, Windows MSVC, iOS
  arm64 + arm64 simulator. The app's link step must pass on the staticlib's `native-static-libs`
  (`c++`, `Foundation`, `CoreML`).
- **`load-dynamic`** (every Android ABI, Intel macOS, x86_64 iOS sim, musl, ...): loads
  `ORT_DYLIB_PATH` or the system library. A missing library or one older than 1.17 is
  `ModelLoadFailed`, never a panic. Android ships the `onnxruntime-android` AAR (keep it on 1.28.x).
  Android arm64 stays dynamic on purpose (static would need `libc++_shared.so`).

### Local database (`tools/sql/sqlite.rs`, `migrations/`)

Single-connection pool (one writer). In-memory DBs use a uniquely named shared cache plus a
keeper connection. Files are `0600` with `secure_delete`. `wipe` ends with `VACUUM`, skips FTS5
shadow tables and clears virtual tables last with `'delete-all'`.
