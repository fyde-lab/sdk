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

- `mod.rs` — public domain types (`Document`, `NewDocument`) and the `Service` trait (`save`/`upload`/`download`/`download_many`/`get`/`list`), the business-level API other code depends on. Trait methods take `&self` (not `&mut self`) so implementations can be shared behind `Arc<dyn Service>`.
- `grpc_client.rs` — thin gRPC transport (`GrpcClient`). Knows nothing about documents semantics; just sends proto requests and returns raw proto responses. Methods take `&self`, cloning the underlying tonic `Channel` per call (cheap, safe to use concurrently) since the generated client's RPC methods require `&mut self`. Generated protobuf bindings are compiled from `../api-protos/documents.proto` by `build.rs` (via `tonic-prost-build`) and included with `tonic::include_proto!("documents")`.
- `crypto.rs` — envelope encryption primitives (AES-256-GCM). Generates a fresh per-document data encryption key (DEK), encrypts content and metadata under it, and wraps the DEK under a key-encryption-key (KEK) before anything leaves the process. The server only ever stores/returns ciphertext and a wrapped DEK — it cannot read document content or metadata (name, content type, etc.).
- `service.rs` — `DocumentsClient`, the default `Service` implementation, which composes the crypto and gRPC layers and delegates local, unencrypted persistence to an injected `Storage`. `download`/`download_many` are single round-trips against the server: the server's `FetchDocument`/`FetchDocuments` RPCs return `EncryptedDocument`s (ciphertext content, wrapped DEK, and encrypted metadata together), so no separate lookup is needed to decrypt them. `get` and `list`, unlike `download`, never talk to the server — they read documents previously persisted by `save` straight out of the injected `Storage`; `list` pages through them oldest-first via `offset`/`limit`. There is no listing RPC — `ListDocuments` was removed from `documents.proto`, so `list` only ever sees documents already cached locally by a prior `download`/`download_many`.

**Known temporary state**: `crypto.rs` derives its KEK from a hard-coded placeholder secret (`TEMP_HARDCODED_KEK_SECRET`), explicitly marked in a doc comment as needing replacement with a real KMS/HSM/secrets-manager-sourced key before handling real data. Don't remove that comment when touching this file unless the underlying issue is actually fixed.

### Changelog service (`src/services/changelog/`)

Same three-piece layering as documents:

- `mod.rs` — the `Service` trait (`consume`), `&self` for the same `Arc<dyn Service>`-sharing reason as `documents::Service`. `consume` takes a boxed `FnMut(ChangelogEvent) + Send` callback (not a generic parameter) so the trait stays object-safe for `Arc<dyn Service>`.
- `grpc_client.rs` — thin gRPC transport (`GrpcClient`), also `&self`-per-call via cloning the generated client's `Channel`. Generated bindings come from `../api-protos/changelog.proto`.
- `service.rs` — `ChangelogClient`, the default `Service` implementation. `consume` opens a `WatchEvents` subscription and, for every event it receives, pages through `ListEventsSince` starting from the cursor persisted in an injected `Storage`, advancing that cursor after each page; the callback is invoked once per event during both the initial catch-up and the live subscription. `list_since` and the private `catch_up` helper are inherent methods, not part of the trait, since nothing outside `consume` calls them.

### Layering: simplified hexagonal architecture

Across `documents` and `changelog`, the layer boundary is strict: **all business logic — validation, orchestration, cursor/offset bookkeeping, business rules — lives in `service.rs` and nowhere else.** `grpc_client.rs` is a transport adapter: it only sends proto requests and returns raw proto responses, no business rules. `crypto.rs` is a crypto adapter: it only performs envelope encryption/decryption, no business rules. Storage (`SqliteClient`/injected `Storage`) is a persistence adapter: it only reads/writes rows, no business rules. If you find yourself validating input or making a business decision inside a transport, crypto, or storage file, move it into `service.rs`.

### Testing: `service.rs` tests use `mockall`, not real implementations

**`service.rs` business logic (`DocumentsClient`, `ChangelogClient`) must be tested only against `mockall`-generated mocks of its injected dependency traits — `Storage`, `documents::Service`/`changelog::Service` where used cross-domain, etc. — never against a real `grpc_client.rs`, `crypto.rs`, or `SqliteClient` implementation.** Mark each dependency trait `#[async_trait]` plus `#[cfg_attr(test, mockall::automock)]`, build the service under test from the generated `Mock*` type, and fake persistence semantics with `.returning()` closures (e.g. a `Mutex`-backed map/list), following the same pattern used in `../server/CLAUDE.md`. `grpc_client.rs`, `crypto.rs`, and `src/lib/sql/sqlite.rs` are the exception: their own tests exercise the real transport/crypto/SQLite adapter, which is correct there — never in `service.rs`.

### SQLite client (`src/lib/sql/sqlite.rs`)

`SqliteClient` manages a local on-disk database independent of the gRPC services, at `$XDG_DATA_HOME/fyde/fyde.db` (falling back to `~/.local/share/fyde/fyde.db`), created on first connect. Uses a single-connection pool deliberately, since SQLite only supports one writer at a time.

### Errors

All fallible SDK operations return the crate-wide `Result<T> = Result<T, Error>` (`src/lib.rs`). `Error` is a single enum covering every failure domain (gRPC transport/status, UUID parsing, I/O, sqlx, XDG, JSON, encryption) via `thiserror` `#[from]` conversions — add new variants there rather than introducing per-module error types.

**Every error must be wrapped with a context message before it's returned** — never a bare `?` with no explanation of what was being attempted. Use the crate-wide `ErrorContext` trait (`src/lib.rs`) at each fallible site: `.context("failed to open local database")` or `.with_context(|| format!("failed to write changelog offset {offset}"))`. This wraps the error in `Error::Context { message, source }`, preserving the original as `#[source]`. See `src/lib/sql/sqlite.rs` for examples.
