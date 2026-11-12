# CLAUDE.md

`fyde-sdk` is a Rust client library (no binary) for a fyde server (`../server`). It's built
standalone (not a Cargo workspace member); `../scripts`, `../cli` and `../application` (via
its own native bindings crate, `../application/sdkBridge/rust`, which mirrors this crate's
public API: update it with any API change) consume it. Protos come from the Buf Schema Registry
(`buf.build/fyde-lab/api`, published from `../server/protos`).

## Commands

```sh
cargo build
cargo test                       # or: cargo test <name>
cargo clippy --all-targets -- -D warnings   # what CI runs (with cargo fmt --check, cargo test)
cargo fmt
FYDE_PROTO_DIR=../server/protos cargo build  # build without buf / network access
RUSTFLAGS="--cfg tokio_unstable" cargo build --features tokio-console  # serve tasks to tokio-console (127.0.0.1:6669)
```

- Dev shell: `shell.nix` via direnv. Rust **1.98+** (`wreq`'s MSRV); `protobuf`; `cmake`/`perl`/
  `go`/`clang` for `wreq`'s vendored BoringSSL; `webkitgtk_4_1`/`gtk3` for `wry`.
- `build.rs` runs `buf export buf.build/fyde-lab/api` (needs `buf` on `PATH` + network) unless
  `FYDE_PROTO_DIR` is set.
- On static-ONNX-Runtime targets (see "ONNX Runtime" below), `ort-sys` downloads pyke's prebuilt
  library on the first build (network, then cached); `ORT_LIB_PATH` points it at a local build.
- `FYDE_TEST_MODELS_DIR=/some/dir cargo test -- --ignored multilingual_e5` runs the real-model
  tests (`tools::onnx` and `documents::embedder`; downloads ~135 MB from Hugging Face; the directory keeps them for the next run).
  `-- --ignored gliner_x_small` runs `documents::gliner`'s (~190 MB).

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
  `ErrorCode`, and is exported so the application's bindings generate their error type from
  it too) plus wording in every language in `messages.rs`.
- `ErrorCode` values (`1xxx` device, `2xxx` server, `3xxx` account, `4xxx` documents, `5xxx`
  scripts) are persisted by callers: **never renumber or reuse one**.

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
- A new secret must be added to `secrets::ALL_SECRETS`, or logout leaves it behind.
- Key material (master key, DEKs, KEKs, OPAQUE export keys, recovery keys and entropy) and
  every value read from `secrets` are held in `zeroize::Zeroizing` (wiped on drop), the
  `secrets` cache included. A type carrying a secret (session token, script parameters)
  implements `Debug` by hand and redacts it: never derive it. Secrets
  (master key, session token) never go in the `settings` table.
- `logout` always clears secrets, wipes the database and deletes every other file in the
  client's directory (`tools::client_dir`; only the open database's files and the changelog
  lock stay), whether or not the server answers. Anything a domain writes to disk must live
  in that directory, or logout leaves it behind. Plaintext-derived data held in memory (the
  documents `EmbeddingCache`) is cleared by `users`' `clear_local_state` (logout and session
  expiry), after the secrets: an upload caching its embeddings later can't publish, and
  drops them. Never log embedding vectors or document text.
- Every Lua VM is built with `tools::sandbox::new_vm` and scripts loaded with
  `sandbox::load_source` — never `Lua::new*` / `lua.load` (that would run precompiled bytecode).
- Scraper debug reports never record request headers, request/download bodies, form values or
  `fyde.input.ask` answers; response bodies only with `debug_http_dump`. Every URL a report or
  log line carries goes through `reports::redact_url` (no query, fragment or userinfo), and
  `wreq` errors are recorded `without_uri()`.
- On-device model files (`tools::models`) live in `ClientConfig::models_dir`, never the client
  directory, and only ever land there through `ModelStore::ensure` (size + SHA-256 checked, then
  renamed into place). Pin a new model to a fixed revision, never a branch. `Client::init`
  itself never waits on a download: it only starts every model's (embedding and GLiNER) in
  the background through `documents::ModelPreloader` (one task per model, files only).
  `start_loading` loads them in memory the same way (downloading what's missing): called by
  `users::create` and by the app on opening the sources page (`Client::start_loading_models`).
  Neither starts again while its previous run is still going (aborting it would restart a
  download); dropping the `Client` aborts both; a failure is logged at warn, the first use
  trying again. Unit tests that `init` a
  `Client` pass a regular file as `models_dir` so that download fails without the network.
- Never write to the `documents_fts` FTS5 table directly (triggers maintain it). A new virtual
  table must support `'delete-all'` so `SqliteClient::wipe` can clear it.

## Testing rules

- **`service.rs` tests use only `mockall` mocks** of injected traits, never a real
  `grpc_client.rs`, `crypto.rs` or SQLite storage. Mark dependency traits `#[async_trait]` +
  `#[cfg_attr(test, mockall::automock)]` (convert `-> impl Future` signatures to `async fn`).
  Configure each mock inside its test with exactly the expected calls (an unexpected call
  panicking is often the assertion) — no shared fake-storage builders. A small inline
  `Arc<Mutex<_>>` capture is fine when a test must read back a generated value.
- Exceptions, which test real adapters: `grpc_client.rs`, `crypto.rs`, `tools/sql/sqlite.rs`, `tools/client_dir.rs`,
  `tools/models` (a local `TcpListener` HTTP server), `tools/onnx` (a hand-encoded ONNX model; the
  real-model test is `#[ignore]`), `documents/embedder.rs` (pooling on hand-built outputs; the
  real-model ranking test is `#[ignore]`), `documents/gliner.rs` (splitting, windows, inputs and
  logit decoding are pure functions tested directly; the real-model test is `#[ignore]`),
  `storage_sqlite.rs` files, `secrets/storage_keystore.rs` (keyring's in-memory mock),
  `changelog/storage_settings.rs`. `changelog::Service` can't be automocked (its
  `start_consume_job` takes a boxed `FnMut`), so use the hand-written `RecordingChangelog`-style
  fakes for it.
- Fixtures: entity structs returned by a domain have a `#[cfg(test)]` `Fake*` builder in the
  same `models.rs` (`FakeDocument::new().with_x(..).build()`; see `FakeDocument`,
  `FakeMetadata`, `FakeChangelogEvent`, `FakeScript`, ...), using `tools::testing`'s random
  helpers. Use them instead of struct literals. Keep them `pub(crate)` and re-export one only
  when another file needs it (unused re-exports fail clippy). Exception: the builders other
  crates' tests need are gated `#[cfg(any(test, feature = "testing"))]`, `pub`, and re-exported
  from `fyde_sdk::testing` (the `testing` feature, for `[dev-dependencies]` only).
- No `tests/` here: the end-to-end suites driving a real `Client` against a real server live in
  `../server/tests/` (which dev-depends on this crate). Keep the API they use public (e.g. the
  `#[doc(hidden)]` `use_in_process_keystore_for_tests`), and update them with any API change.

## Architecture

`Client::init(ClientConfig)` (`src/lib.rs`) opens a lazy gRPC channel to `config.url`
(rustls + bundled webpki roots for `https://`), opens the SQLite database selected by
`config.storage` (`Storage::Disk(dir)` → `<dir>/<client_name>/fyde.db`, `Storage::Memory`) and
wires every domain. `client_name` must be a safe single path segment (`InvalidClientName`).
Before opening a disk database, `init` takes an exclusive `flock` on `fyde.db.lock` beside it,
held by the `Client` until dropped: a second client on the same directory fails with
`ClientAlreadyRunning` (one instance at a time, so every domain may write local storage freely).
Public accessors: `documents()`, `folders()`, `attributes()`, `users()`, `settings()`, `server_state()`,
`scripts()`, `icons()`, `scrapers()`, `usage()`, `user_message(&err)`, `language()`, plus
`save_new_session(token)`/`is_connected()` over the crate-private `sessions` domain, and
`start_loading_models()`.
`ClientConfig::models_dir` is where on-device models are downloaded (see `tools::models`): `init`
doesn't touch it, but rejects one inside a disk client directory (`InvalidModelsDirectory`,
1013; lexical comparison of absolute paths). `ClientConfig` carries the
caller's callbacks (`on_log`, `on_document_change`,
`on_scraper_progress`, `on_scraper_question`).

New server services follow the same pattern: a module under `src/domains/` with `mod.rs`
(the `Service` trait, methods take `&self` so it can live behind `Arc<dyn Service>`),
`models.rs` (plain structs), `service.rs`, and adapters; wired into `Client::init`.
Non-domain helpers live in `src/tools/` (`client_dir`, `hex`, `log`, `models`, `onnx`, `sandbox`,
`sql`, `testing`).

### Domains (`src/domains/`)

- **documents** — no gRPC transport: every write is a `changelog` event. `upload` needs a
  master key, rejects content over `changelog::MAX_EVENT_BYTES` (`DocumentTooLarge`, 4014: the
  server decodes with tonic's 4 MiB default) and a known checksum (`DuplicateDocument`), both
  before parsing; `changelog::Service::send` re-checks the encrypted event's size, parses, then
  publishes an encrypted `SaveDocument` event; the document reaches the local cache only when
  the sync job consumes it back. `delete` likewise only publishes a `Deleted` event (id only,
  no local check, so an unsynced upload can be deleted); consuming it removes the cached
  document through `Indexer::delete_document` (embeddings cascade, FTS triggers follow), a
  no-op if it isn't cached. `get`/`list`/`search`/`search_facets` are local-only
  (`storage_sqlite.rs`, FTS5 `bm25` ranking over contentless `documents_fts`). `list`/`search`
  return `Metadata` only (their query never selects `content`; `size` is `length(content)`):
  `get` is the only call that loads a document's content, for when it's actually shown.
  `search_facets` counts each field with its own filter lifted so UIs only offer reachable
  choices. A document's place is `Metadata::folder_id` (`None` = the root, À classer; a
  folder deleted meanwhile counts as the root too): `upload` takes only the source and an
  optional name (the classifier guesses the type, the document lands at the root),
  `move_document`/`update_metadata` refuse a folder not synced here (`FolderNotFound`).
  `SearchFilters::folder` is `FolderFilter::Unfiled` or `Within(id)` (subfolders included,
  a recursive CTE over `folders`); `SearchFacets::folders` counts each folder with everything
  below it (`folder_facets` folds storage's direct counts up the tree), plus `unfiled`. The
  folders domain's `Applier` moves or deletes a deleted folder's documents locally
  (`move_folder_documents`/`delete_folder_documents`).
  - `attributes/` — typed per-document values (`Metadata::attributes`, name → `Attribute`;
    amounts are `i64` minor units, never floats) and their server-side *definitions*
    (`AttributesService::list`/`set`/`check`). Checks (`check_values`, then defined types) run on
    upload and `update_metadata`. Promote anything that needs filtering/sorting to a real
    `Metadata` field instead of querying attributes JSON.
  - `transcript.rs` — extracts a PDF's text into `Metadata::transcript` on upload (and for `fyde.pdf`, whole or within a rectangle).
  - Indexing: a document is never cached without its embeddings. The changelog consumer
    applies `SaveDocument`/`UpdateMetadata` through the `Indexer` trait (`DocumentIndexer` in
    `service.rs`), which embeds then writes document + embeddings in one transaction. Rules:
    `chunk.rs` splits the transcript (pages, then paragraphs, then lines; no overlap), each
    passage is `"{title}\n{chunk}"` (title = name without extension); a blank transcript is
    indexed with zero chunks; `update_metadata` re-embeds only if the title or transcript
    changed. `embedder.rs` is the ONNX adapter (multilingual-e5-small, `passage: `/`query: `
    prefixes, mean pooling, L2-normalized, 384 dims, 512-token truncation, `spawn_blocking`
    on ≤4 threads, loaded lazily once; a failed load is retried). Table `document_embeddings`
    (`(document_id, chunk)` key, `vector` = 384 little-endian `f32`, cascades with its
    document, cleared by `wipe`). `Indexer::prepare` fetches/loads the model; its last failure
    is `Service::indexing_model_error` (for the UI). One `OnnxEmbedder` (`init_embedder`) is
    shared by the indexer and searches.
  - Upload-time embeddings: `upload` embeds the document (same `passages`, chunker and
    embedder as the indexer, waiting for the model like it) before publishing, so the upload
    flow has the vectors before emission. They never travel in the event, and `upload` writes
    no table: it `put`s them in the in-memory `EmbeddingCache` (`init_embedding_cache`, one per
    `Client`, shared by the documents service, the indexer and `users`; `embedding_cache.rs`
    only stores: 64 MiB of entries, oldest put evicted first, never persisted) and removes
    them unless it published (`CachedUntilPublished`: a failed publish, or an upload dropped
    while publishing). A blank transcript or a failed embed (logged, never fatal)
    publishes without caching; after a failed embed, uploads skip embedding for
    `UPLOAD_EMBED_RETRY_AFTER` (5 min) rather than each wait for the model again. A
    duplicate the consumer skips goes through `Indexer::skip_document`, which drops its
    entry. Entries are keyed by `embedding_key`: document id + model +
    `CHUNKER_VERSION` + SHA-256 of the length-prefixed passages (**bump `CHUNKER_VERSION`
    whenever `chunk.rs` or the passage format changes**). The indexer's `save_document` stores a
    matching entry's vectors as-is, else embeds inline, and removes the entry only once the
    transaction committed (a failed pass retries with it).
  - Hybrid search: `search` with `Relevance` and non-blank text fuses (in `service.rs`) two
    storage rankings over the filtered documents, top `CANDIDATES` (200) each: `rank_by_terms`
    (bm25) and `rank_by_similarity` (cosine to the query, each document scored by its best
    chunk, kept only at or above `SIMILARITY_FLOOR`, 0.85, tuned on a real corpus: e5 puts
    unrelated text around 0.8). Fusion is RRF (Σ 1/(60 + rank)), then the page is cut and only
    its `Metadata` loaded (`get_metadatas`). `Embedder::embed_query` (`query: ` prefix) uses
    `ModelStore::find`, which never downloads; if it fails the search logs and falls back to
    bm25 alone, never failing over the model. `search_facets` with non-blank text counts the
    same hybrid set (`count_documents_among`/`created_at_range_among`). `Newest`/`Oldest` and
    blank text stay bm25/filters only.
  - Similar documents: `similar(id, min_confidence, limit)` reads the document's stored vectors
    (`get_embedding_vectors`, current model only; none = no results, never embeds), averages
    them, and ranks the rest with `rank_by_similarity` (unfiltered, itself dropped). The
    cosine is rescaled to `SimilarDocument::confidence` 0..=1 from `UNRELATED_SIMILARITY`
    (0.8, untuned on document pairs; below it a document is never returned) up to 1.0, so
    callers never handle e5 numbers. `DEFAULT_SIMILAR_MIN_CONFIDENCE` (0.25) maps back to
    `SIMILARITY_FLOOR` (a test checks): keep the two in sync if either moves.
  - `classification/` — zero-shot `DocumentType` and folder guess at upload (ported from
    `doc-ai-poc`'s `classify.rs`): the whitespace-collapsed transcript (first 16 KiB) embedded
    as one passage, against `query: `-embedded texts. Types: French descriptions per type
    (`TYPES`: business data, kept there; a type scores its best description), flat argmax
    (`Other` has no description) needing `TYPE_MARGIN` (0.01) over the runner-up. Folders: each
    described folder's `"{name} : {description}"` (a folder without a description is never
    picked), the best one needing `FOLDER_MARGIN` (0.01) over the runner-up and at least
    `FOLDER_FLOOR` (0.83). Both margins, the floor and the proposed layout's French
    descriptions were tuned on a private corpus of ~1,200 real French documents labelled with
    their proposed-layout folder (kept outside every repo); descriptions say how such documents
    read, generically, never naming one household's issuers. Cross-language (English layout,
    French documents) scores run ~0.04 lower, so few such documents clear the floor. The
    `#[ignore]`d real-model test files synthetic documents in the French proposed layout.
    Type descriptions are embedded lazily, once per process and model (`DESCRIPTIONS`); folder texts are cached
    per model, keeping only the latest classification's. `upload` classifies only after its
    embedding succeeded (model at hand), against the folders synced here; a failure publishes
    untyped at the root. Never run on consumption.
  - `gliner.rs` — `SpanFinder` (`OnnxGliner`, GLiNER-X small; ported from `doc-ai-poc`'s
    `gliner.rs`), a pure adapter not wired into any flow yet (only the `ModelPreloader`
  fetches and loads its model): `spans(text, labels, threshold,
    max_words)` returns every span (byte range, label index, sigmoid score) scoring at least
    `threshold`, best first, overlaps and window-boundary duplicates included. Keep the model's
    details as they are: `<<ENT>>` = 250100 / `<<SEP>>` = 250101 (not in `tokenizer.json`),
    `MAX_WIDTH` 12, 320-word windows overlapping by 12, `split_words` mimicking GLiNER's
    `\w+(?:[-_]\w+)*|\S`, only the inputs the model declares passed (an unknown one fails), logits
    checked to be `[1, words, 12, labels]`. Loaded lazily on the first call with something to
    look for (a failure is retried), run on the blocking pool.
  - `dev.rs` — `DevDocumentsClient`, plain-file writes for `init_dev_scrapers`.
- **changelog** (crate-private) — the server is a blind relay of client-encrypted events
  (`SaveDocument`, `UpdateMetadata`, `Deleted`, `InstallScript`, `UninstallScript`, `SaveFolder`, `DeleteFolder`). `crypto.rs`: AES-256-GCM, per-event DEK
  wrapped by a KEK derived from the master key. The consume job replays from the persisted
  cursor then tails live, materializing documents and script installations (an `InstallScript`
  whose script version differs from the installation's previous one first deletes its saved
  `scraper_sessions` row, keyed by installation id, so the new version refetches everything;
  an `UninstallScript` deletes the installation and its sessions, cookies and reports through
  `scrapers::StateStorage`). Each event's
  `previous_id` (the server chains a user's events, empty for their first) must equal the last
  event the pass went through (starting at the cursor), checked before decrypting: otherwise
  the pass fails rather than skip past a gap or apply another account's event. A failed pass is
  retried after a backoff (500 ms doubling to 30 s, reset by a clean pass); a stream that ends
  cleanly is reopened after 1 s, never at once. Applied events are
  recorded by ciphertext digest (`applied_changelog_events`), so one the server serves again is skipped.
  Documents are applied through `documents::Indexer` (a checksum duplicate is skipped, never
  embedded); before a pass's first document event it calls `prepare`, so a missing model
  fails the pass (retried) rather than letting the cursor pass an unindexed document. An
  event failing `POISON_PILL_PASSES` (5) passes in a row is logged at error level on each
  further failure; it's never skipped (ask before adding any skip mechanism).
- **folders** — the vault's folders (`Folder`: parent, name, description, and a colour + icon
  on top-level folders only). No gRPC transport: `Service` validates each change against the
  folders synced so far (name trimmed, 1–60 chars, no control chars, unique among siblings
  case-insensitively; description ≤ 300 chars; appearance required at the top level and refused
  below; no move into itself or a descendant) and publishes a `SaveFolder` (the whole folder,
  JSON) or `DeleteFolder` (`FolderContents::Keep`/`Delete`, JSON) event; `list` reads the local
  `folders` table. The changelog consumer applies them through `Applier` (`FolderApplier`),
  which resolves concurrent edits identically on every device: a missing parent or a cycle
  puts the folder at the top level (default appearance if it has none), a sibling name clash
  gets a `" (2)"` suffix; deleting keeps the children (moved up, taking the top-level
  appearance when they land at the root) or removes every descendant.
  `create_proposed_layout(language)` publishes `layout.rs`'s `PROPOSED_LAYOUT` (16 top-level
  folders with appearance, one subfolder per kind of paper, French or English names and
  descriptions; top-level folders with subfolders have no description, so the classifier files
  into the subfolders), reusing a same-named folder already there. Keep its texts within the
  folder rules (tests check).
- **users** — OPAQUE (RFC 9807, `opaque-ke`); the password never leaves the SDK.
  `FydeCipherSuite` (ristretto255 + 3DH/SHA-512 + Argon2id 64 MiB/3 iters/4 lanes) must match
  `../server`'s. `crypto.rs`'s `OpaqueClient` trait exists so service tests skip real KSF cost. Its `finish_*`
  steps run Argon2id, so `service.rs` calls them on the blocking pool (`spawn_blocking`), never
  inline on an async worker.
  The master-key wrapping key is HKDF'd from the OPAQUE export key. A wrong password usually
  fails locally (`InvalidCredentials`); the SDK then still closes the server's pending exchange
  (`login_id`, single-use) with a decoy finalization that can't verify (`abandon_login`), as
  `change_password` always does after checking the old password, so none is left behind. One account per local database (`AlreadyLoggedIn`).
  `create`/`login`/`recover_account` cache the username (`USERNAME_SETTING`) and role locally;
  `login` persists the session token last. `login` returns `LoginOutcome::LoggedIn` or
  `SessionLimitReached { sessions, .. }`; the caller re-runs the whole login with
  `replace_session_id` (the server's `login_id` is single-use). `list_sessions`/`revoke_session`
  manage open sessions (never cached). `analyze_password` rates a password with the
  `passwd-strength` crate (score, `PasswordStrength`, length in chars; the crate's English-only
  tips are dropped); `create`/`change_password` refuse a `Weak` new password first
  (`WeakPassword`, 3004), before any other check or server call. `change_password` takes only
  the old and new passwords, verifying the old one against the cached username
  (`UsernameUnknown`, 3006, for a session opened before it was cached: sign in again).
  Recovery keys (`crypto.rs`): a 24-word English BIP39 mnemonic (256-bit entropy, `bip39`
  crate), never stored. HKDF-SHA256 over its entropy derives an auth key (sent to the server,
  which keeps a digest) and a wrapping key for the master key. `generate_recovery_key`
  replaces the account's one (at most one); `delete_recovery_key`/`recovery_key_status`.
  `recover_account` (no session; parses the phrase locally, case/spacing-insensitive, and
  refuses a `Weak` new password first) re-wraps the recovered master key under the new
  password and opens a session. A server `UNAUTHENTICATED` there is `InvalidRecoveryKey` (3005).
- **secrets** (crate-private) — master key and session token in the OS credential store
  (`keyring-core` + per-platform stores; only Linux/Secret Service has been exercised), entries
  namespaced per database path. If no store is usable, `create`/`login` fail with
  `SecureStorageUnavailable` unless called with `allow_insecure_storage`, which writes to the
  `insecure_secrets` table. Android apps must call `io.crates.keyring.Keyring`'s
  `initializeNdkContext(context)` before the first `Client::init`.
- **sessions** (crate-private) — `authenticated_request`, `check_response` (see rules above;
  the `ExpiredSessionHandler` wipe runs on its own spawned task), `is_connected`.
- **scripts** — reads scripts from `scripts.v1` (`fetch_script`, `list_user_scripts`,
  `list_public_scripts`) and enables/disables them for the user. Writing scripts and icons is
  admin-only on the server (`admin.v1`) and not in this SDK: `../scripts`' `publish` calls the
  admin API itself. A user can install a script several times, once per account on its site:
  each *installation* (`InstalledScript`) has its own id (a new one unless `install_script` is
  given one to replace) and a label (trimmed, ≤ `MAX_INSTALLATION_LABEL_CHARS`, no control
  chars, `InvalidInstallationLabel` 5012; a blank one defaults to the first non-blank
  non-secret text parameter, `username` first then by name, else the script's name). `install_script` publishes an
  encrypted `InstallScript` event (subject: the installation id), `uninstall_script` an
  `UninstallScript`; installations are read from the local `installed_scripts` table (keyed
  by installation id). Enabling/disabling (and the plan's `max_scripts` quota) stays per
  script on the server, which never sees installations: callers enable on install and
  disable once a script's last installation is gone.
- **icons** — read-through cache over the server's `icons.v1.IconsService`: `fetch_icon(id)`
  serves the local `icons` table, else fetches and caches (icons never change, so never
  revalidated).
- **settings** (key/value, non-secret; `Client::settings` hands callers `settings::public`,
  which stores their keys under `app.` so they never reach the SDK's own), **server_state**
  (gRPC health; never fails), **usage** (`get_usage`, plan limits + current usage; never cached).
- **scrapers** — runs Lua scripts that log into third-party sites. Contract: the script
  returns `{ run = function(parameters) ... }` and only calls the `fyde` table built in
  `host/` (`http`, `html`, `json`, `pdf`, `log`, `progress`, `input`, `save_document`, `session`,
  `browser`, plus `helpers`: shared Lua helpers in `host/helpers.lua` that scripts alias instead
  of inlining — `../scripts` relies on their names and behaviour).
  - `mlua::Lua` is `!Send`: `run` builds the VM inside `spawn_blocking` and host functions use
    a captured runtime `Handle::block_on`. Never move a `Lua` across threads.
  - Runs are parallel and isolated: each gets its own VM (25 MiB, `LUA_MEMORY_BYTES`; response
    bodies are capped below it at 16 MiB, `host::MAX_BODY_BYTES`), cookie jar, session table,
    recorder and webview. At most `MAX_CONCURRENT_RUNS` (3) run at once; the others wait for a
    slot (stoppable), taken only once the run holds its name. Runs sharing a `name` share persisted state, so `ScrapersClient` serializes
    them (`lock_name`). `on_scraper_progress`/`on_scraper_question` get the run's name first
    so callers can tell concurrent runs apart. Never add process-wide mutable state a script
    can reach.
  - `fyde.progress` carries no free text, so the UI can word it in the user's language:
    `step(name)` only accepts a `ProgressStep` name (`"logging_in"`, ...; anything else raises),
    each also exposed as a constant scripts use instead (`fyde.progress.LOGGING_IN`),
    `update(current, total)` has no message. New steps need wording in every language in
    `ProgressStep::user_message`, plus a matching variant in the application's bindings
    (`../application/sdkBridge/rust`).
  - `pdf.text(content)` / `pdf.text_in_rect(content, page, {x, y, width, height})` read a PDF's
    text through `documents::transcript` (`page` from 1; points from the bottom-left corner).
  - `save_document` uploads via `documents::Service::upload`; `DuplicateDocument` is skipped.
    `helpers.save_pdf` records each URL it saved from in `fyde.session.saved_urls`
    (`helpers.already_saved`/`mark_saved`) and skips it on later runs without downloading,
    unless called with `{ refresh = true }` (a fixed URL whose document changes). Installing
    a new script version clears the session, so its next run fetches everything again.
  - `session/`, `cookies/`, `reports/` persist per scraper through a `StorageConfig`
    (`Sqlite` for a `Client`, `File` for `init_dev_scrapers`). `fyde.session` is saved even
    on failure; cookies only on success or stop (a failed run deletes them).
  - `stop(name)` stops the run under `name` (and one queued behind it) through its
    `StopSignal` (`stop.rs`): the sandbox's instruction hook (`new_vm`'s `interrupt`), every
    blocking host call (`StopSignal::block_on` instead of a bare `Handle::block_on` — keep it
    that way in new host functions), a pending `fyde.input.ask` (its handler runs on the
    blocking pool and is abandoned) and an upload (`save_document` is given up on, even
    while it waits for the embedding model or publishes: one given up on while publishing
    may still be published). The run ends with `ScraperRunStopped` unless its script
    finished first.
  - `allowed_domains` fails closed: an empty list allows no host; an IP literal only matches
    exactly.
  - `browser/` — `wry`/`tao` webviews on one process-wide thread, started lazily (GTK panics if
    initialized from a second thread; all its objects are `!Send`). Each run gets its own window
    and incognito webview, addressed by a session id in every `EventLoopProxy` command; IPC
    replies go to per-run slots. Only verified on
    Linux/WebKitGTK. Webview cookies are merged into the saved jar (browser wins) and restored
    as domain cookies (`driver::to_webview_cookie`). Forms are submitted with `click`, never
    an in-page `fetch` (it bypasses bot-mitigation hooks); `download` cancels the native
    download and re-fetches its URL in-page, so its bytes never touch the disk.
  - A failed run keeps a code the UI can act on (`error::lua_code`): a host error keeps its own
    (e.g. `QuotaExceeded` from `save_document`), even rethrown by the script; a connect or
    timeout failure is `ScraperSiteUnreachable`; the login contract's `error("INVALID_CREDENTIALS", 0)`
    / `error("MFA_FAILED", 0)` are `ScraperCredentialsRejected` / `ScraperVerificationFailed`;
    the sandbox's memory, instruction and body-size limits are `ScriptLimitExceeded`; a stopped
    run is `ScraperRunStopped`; anything else is `ScriptFailed`.
  - `run_installed` runs an installation under its installation id as the run's `name`, so each
    account has its own session, cookies and reports, and runs in parallel with the others
    (`wreq_emulation`/`follow_redirects` default on).
  - `crate::init_dev_scrapers` builds a scrapers service without a `Client` (file storage +
    `DevDocumentsClient`); `../scripts` is its caller.

### On-device models (`src/tools/models/`, `src/tools/onnx/`)

Used by `documents::embedder` and `documents::gliner`. `tools::models` is the registry: each `Model`
is one file (name, URL at a fixed Hugging Face revision, size, SHA-256); today
`MULTILINGUAL_E5_SMALL` (MIT) and `GLINER_X_SMALL` (Apache-2.0, `knowledgator/gliner-x-small`),
both int8 ONNX, each with its `_TOKENIZER`. `ModelStore::ensure` returns `<models_dir>/<sha256[..16]>-<name>`. A file
already there is SHA-256-checked the first time the store sees it (replaced on mismatch), then
trusted by size. Otherwise it's downloaded over `wreq` (client built on the first download;
redirects followed) into a `.<file>.<uuid>.part` beside it, removed on any failure or
cancellation, then renamed. A per-file lock makes concurrent `ensure`s download once. Before
downloading, `ensure` removes the model's other revisions (`<other hash>-<name>`) and its stale
partials of any revision (10 min untouched; vanished files are fine). `find` returns a model
already fully there (same checks) or `None`, never downloading nor waiting on a download's lock.
Errors: `ModelDownloadFailed` (1201, transport, 408/429/5xx, and `ModelNotDownloaded` for a
model `find` lacks), `ModelUnavailable` (1204, any other HTTP
status: only an update helps), `ModelCorrupted` (1202, wrong size/hash), `ModelLoadFailed`
(1203, `tools::onnx`, and `ErrorKind::InvalidModel` for a model whose inputs/outputs don't match).
`tools::onnx` is a thin port of `doc-ai-poc`'s loaders: `session(path, threads)` (Level3
optimizations) and `tokenizer(path)` (`tokenizers`, `onig` regexes).

### ONNX Runtime

`ort` is pinned to `=2.0.0-rc.13`, the exact version `oar-ocr` 0.10.2 uses, so a future OCR
dependency shares the one runtime (add it with `default-features = false`: its defaults enable
`ort`'s `download-binaries` + `tls-native`). No `api-*` feature, so any ONNX Runtime ≥ 1.17 loads.
How it's linked depends on the target, chosen by the two target-specific `ort` entries in
`Cargo.toml` and mirrored by `build.rs`'s `ort_links_statically` (`cfg(ort_load_dynamic)`): keep
all three in sync.

- **Static** (pyke's prebuilt ONNX Runtime 1.28, linked into `fyde-sdk`): Linux gnu x86_64/arm64,
  macOS arm64, Windows MSVC x86_64/arm64, iOS arm64 and the arm64 iOS simulator. The app ships
  nothing extra. iOS/macOS link `c++` and the `Foundation` + `CoreML` frameworks: cargo reports
  them in the staticlib's `native-static-libs`, which the app's link step must pass on (as it
  already does for `Security`). Windows builds link DirectML/D3D12/DXGI/DXCore (system DLLs).
- **`load-dynamic`** (everything else: every Android ABI, Intel macOS, x86_64 iOS simulator, musl,
  ...): `tools::onnx::session` first loads the shared library, `ORT_DYLIB_PATH` if set, else
  `libonnxruntime.so` / `libonnxruntime.dylib` / `onnxruntime.dll` from the library search path;
  missing or older than 1.17 fails with `ModelLoadFailed` (never `ort`'s own panic). Android arm64
  has a pyke static build, but deliberately isn't static: the app ships `onnxruntime-android` for
  its other ABIs anyway, and that build would also need `libc++_shared.so`, which the app doesn't
  ship (its C++ is linked statically).
- **What the app must bundle**: on Android, the official `com.microsoft.onnxruntime:onnxruntime-android`
  AAR (keep it on 1.28.x, matching the static targets), whose `jni/<abi>/libonnxruntime.so` covers
  arm64-v8a, armeabi-v7a, x86 and x86_64 (`../application/sdkBridge/build.gradle.kts` depends on
  it). A load-dynamic Apple target would need `libonnxruntime.dylib` (the `onnxruntime-c`
  xcframework) in the bundle, but the app only builds the static ones.

### Local database (`src/tools/sql/sqlite.rs`, `migrations/`)

Single-connection pool (one SQLite writer). In-memory databases use a uniquely named
shared-cache DB plus a keeper connection, so sqlx replacing a pooled connection never yields an
empty, unmigrated DB. Files are `0600`, `PRAGMA secure_delete = ON`, and `wipe` ends with
`VACUUM`; it skips FTS5 shadow tables and clears virtual tables last with `'delete-all'`.
