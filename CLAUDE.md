# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`fyde-sdk` is a Rust client library (not a binary) for talking to a fyde server. It is one crate in a sibling-project workspace at `../` (alongside `server`, `cli`, and `api-protos`) but is *not* a Cargo workspace member — it's built and versioned standalone. Protobuf/gRPC service definitions consumed by this SDK live in `../api-protos/` and are compiled at build time; `../server` is the corresponding service implementation.

## Commands

- Build: `cargo build`
- Test: `cargo test` (run a single test with `cargo test <test_name>`)
- Lint: `cargo clippy`
- Format: `cargo fmt`
- The dev environment (rustc/cargo/rustfmt/clippy/rust-analyzer) is provisioned via `../shell.nix` + `.envrc` (direnv/nix).

## Architecture

`Client` (`src/lib.rs`) is the SDK entry point, with two constructors: `Client::connect(url)` opens a gRPC connection backed by the local SQLite database in the default XDG data directory, and `Client::connect_memory(url)` does the same but backed by a private in-memory database that only lives for the process's lifetime. Both expose the same service-specific sub-clients (`documents()` → `&DocumentsClient`, `changelog()` → `&ChangelogClient`). As more server services are added, they should follow the same pattern: a submodule under `src/services/`, wired into `Client`.

### Documents service (`src/services/documents/`)

Layered in three pieces, each only aware of the layer below it, following the same `mod.rs`/`service.rs` split as `../server`'s domain modules:

- `models.rs` — the domain-specific structs (`Document`, `Metadata`), with no logic beyond field definitions and derives.
- `mod.rs` — re-exports the domain types from `models.rs`, plus the `Service` trait (`upload`/`get`/`list`), the business-level API other code depends on. Trait methods take `&self` (not `&mut self`) so implementations can be shared behind `Arc<dyn Service>`.
- `grpc_client.rs` — thin gRPC transport (`GrpcClient`). Knows nothing about documents semantics; just sends proto requests and returns raw proto responses. Methods take `&self`, cloning the underlying tonic `Channel` per call (cheap, safe to use concurrently) since the generated client's RPC methods require `&mut self`. Generated protobuf bindings are compiled from `../api-protos/documents.proto` by `build.rs` (via `tonic-prost-build`) and included with `tonic::include_proto!("documents")`.
- `crypto.rs` — envelope encryption primitives (AES-256-GCM). Generates a fresh per-document data encryption key (DEK), encrypts content and metadata under it, and wraps the DEK under a key-encryption-key (KEK) before anything leaves the process. The server only ever stores/returns ciphertext and a wrapped DEK — it cannot read document content or metadata (name, content type, etc.).
- `service.rs` — `DocumentsClient`, the default `Service` implementation, which composes the crypto and gRPC layers and delegates local, unencrypted persistence to an injected `Storage`. `download`/`download_many` are single round-trips against the server: the server's `FetchDocument`/`FetchDocuments` RPCs return `EncryptedDocument`s (ciphertext content, wrapped DEK, and encrypted metadata together), so no separate lookup is needed to decrypt them. `get` and `list`, unlike `download`, never talk to the server — they read documents previously persisted by `save` straight out of the injected `Storage`; `list` pages through them oldest-first via `offset`/`limit`. There is no listing RPC — `ListDocuments` was removed from `documents.proto`, so `list` only ever sees documents already cached locally by a prior `download`/`download_many`.

**Known temporary state**: `crypto.rs` derives its KEK from a hard-coded placeholder secret (`TEMP_HARDCODED_KEK_SECRET`), explicitly marked in a doc comment as needing replacement with a real KMS/HSM/secrets-manager-sourced key before handling real data. Don't remove that comment when touching this file unless the underlying issue is actually fixed.

### Changelog service (`src/services/changelog/`)

Same layering as documents:

- `models.rs` — the domain-specific structs (`ChangelogEvent`, plus the `EventType` enum), with no logic beyond field definitions and derives.
- `mod.rs` — re-exports the domain types from `models.rs`, plus the `Service` trait (`send`/`consume`), `&self` for the same `Arc<dyn Service>`-sharing reason as `documents::Service`. `consume` takes a boxed `FnMut(ChangelogEvent) + Send` callback (not a generic parameter) so the trait stays object-safe for `Arc<dyn Service>`.
- `grpc_client.rs` — thin gRPC transport (`GrpcClient`), also `&self`-per-call via cloning the generated client's `Channel`. Generated bindings come from `../api-protos/changelog.proto`.
- `service.rs` — `ChangelogClient`, the default `Service` implementation. `consume` opens a `WatchEvents` subscription and, for every event it receives, pages through `ListEventsSince` starting from the cursor persisted in an injected `Storage`, advancing that cursor after each page; the callback is invoked once per event during both the initial catch-up and the live subscription. `list_since` and the private `catch_up` helper are inherent methods, not part of the trait, since nothing outside `consume` calls them.

### Layering: simplified hexagonal architecture

Across `documents` and `changelog`, the layer boundary is strict: **all business logic — validation, orchestration, cursor/offset bookkeeping, business rules — lives in `service.rs` and nowhere else.** `grpc_client.rs` is a transport adapter: it only sends proto requests and returns raw proto responses, no business rules. `crypto.rs` is a crypto adapter: it only performs envelope encryption/decryption, no business rules. Storage (`SqliteClient`/injected `Storage`) is a persistence adapter: it only reads/writes rows, no business rules. If you find yourself validating input or making a business decision inside a transport, crypto, or storage file, move it into `service.rs`.

### Testing: `service.rs` tests use `mockall`, not real implementations

**`service.rs` business logic (`DocumentsClient`, `ChangelogClient`, `UsersClient`, `SettingsClient`) must be tested only against `mockall`-generated mocks of its injected dependency traits — `documents::Storage`, `changelog::OffsetStorage`, `settings::Service`, `documents::Service`/`changelog::Service` where used cross-domain, etc. — never against a real `grpc_client.rs`, `crypto.rs`, or `SqliteClient`/`SqliteStorage` implementation.** Mark each dependency trait `#[async_trait]` plus `#[cfg_attr(test, mockall::automock)]` (this includes traits that were previously written with a bare `-> impl Future<Output = T> + Send` return instead of `async fn` — `automock` needs the `async fn` form, so convert the trait, not just add the attribute), and build the service under test from the generated `Mock*` type. **Configure each test's `Mock*` directly in that test** — `.expect_method().withf(...)...returning(...)` for exactly the calls that test's scenario makes — rather than funneling every test through one shared "fake storage" builder function; a call the test doesn't expect (an `expect_*` that's never set up) correctly panics instead of silently succeeding against a generic backing store, which is often the assertion itself (e.g. "never caches a document for a non-`Created` event" needs no `expect_save_document()` at all). Reach for a small `Arc<Mutex<_>>` capture cell inlined in one test, not a reusable builder, only when that test must read back a value the code under test generates itself (e.g. `users::service`'s `create_persists_an_encrypted_master_key_in_settings` capturing what `.set()` was called with, since the encrypted master key isn't known ahead of time). Exceptions, whose own tests correctly exercise a real adapter — never do this in `service.rs`: `grpc_client.rs`/`crypto.rs` (real transport/crypto), `src/lib/sql/sqlite.rs` and every domain's own `storage_sqlite.rs` (real SQLite), and `changelog/storage_settings.rs` (its own `OffsetStorage`-over-`settings::Service` logic, mocking `settings::Service` rather than touching SQLite). One further exception: `changelog::Service`'s `consume` takes `Box<dyn FnMut(ChangelogEvent) + Send>`, which `mockall` cannot `automock` — code that needs to fake `changelog::Service` (e.g. `documents::service`'s `RecordingChangelog`) uses a hand-written fake instead, which is legitimate specifically because automocking is impossible there, not a pattern to reach for elsewhere.

### Testing: `Fake*` builders for fixture data

Every domain struct in a `models.rs` that isn't itself a request/input type passed into a `Service` trait method (i.e. every entity a domain returns rather than takes as a parameter — `Document`, `Metadata`, `ChangelogEvent`, `User`) has a matching `Fake*` builder in the same file, behind `#[cfg(test)]`: `FakeDocument`, `FakeMetadata`, `FakeChangelogEvent`, `FakeUser`. Note `Metadata` gets one too even though `changelog::Service::send` also takes it as a parameter — what decides this is whether the struct is part of a *result* type (`Metadata` is a field of `Document`), not whether it happens to also appear as a parameter somewhere. `FakeX::new()` fills every field with random-but-plausible data (via `src/testing.rs`'s primitives — `random_bytes`, `random_hex`, `random_word`, `random_username`, `random_past_timestamp`, all backed by `Uuid::new_v4()` rather than a dedicated `rand` dependency, even though `rand` is already a regular dependency here — keeps every `Fake*` builder's randomness source consistent with `../server`'s), `.with_<field>(value)` overrides one field at a time, and `.build()` consumes the builder into the real struct. A builder can also expose a convenience method beyond the mechanical `with_*` set when it captures a common test need, e.g. `FakeChangelogEvent::for_document(&Document)` sets `document_id` from an existing fixture document instead of making the caller copy the id by hand. Use these instead of hand-writing struct literals in new tests. `Fake*` types and their builder methods are `pub(crate)`, re-exported from `mod.rs` only when another domain's tests need them (e.g. `documents::{FakeDocument, FakeMetadata}` used by `changelog/models.rs`); don't export one pre-emptively before something outside its own file needs it, since an unused `pub(crate) use` fails the crate's clean-clippy bar.

### SQLite client (`src/lib/sql/sqlite.rs`)

`SqliteClient` manages a local on-disk database independent of the gRPC services, at `$XDG_DATA_HOME/fyde/fyde.db` (falling back to `~/.local/share/fyde/fyde.db`), created on first connect. Uses a single-connection pool deliberately, since SQLite only supports one writer at a time.

### Errors

All fallible SDK operations return the crate-wide `Result<T> = Result<T, Error>` (`src/lib.rs`). `Error` is a single enum covering every failure domain (gRPC transport/status, UUID parsing, I/O, sqlx, XDG, JSON, encryption) via `thiserror` `#[from]` conversions — add new variants there rather than introducing per-module error types.

**Every error must be wrapped with a context message before it's returned** — never a bare `?` with no explanation of what was being attempted. Use the crate-wide `ErrorContext` trait (`src/lib.rs`) at each fallible site: `.context("failed to open local database")` or `.with_context(|| format!("failed to write changelog offset {offset}"))`. This wraps the error in `Error::Context { message, source }`, preserving the original as `#[source]`. See `src/lib/sql/sqlite.rs` for examples.
