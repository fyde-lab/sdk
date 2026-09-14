# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`fyde-sdk` is a Rust client library (not a binary) for talking to a fyde server. It is one crate in a sibling-project workspace at `../` (alongside `server`, `tui`, and `api-protos`) but is *not* a Cargo workspace member — it's built and versioned standalone. Protobuf/gRPC service definitions consumed by this SDK live in `../api-protos/` and are compiled at build time; `../server` is the corresponding service implementation.

## Commands

- Build: `cargo build`
- Test: `cargo test` (run a single test with `cargo test <test_name>`)
- Lint: `cargo clippy`
- Format: `cargo fmt`
- The dev environment (rustc/cargo/rustfmt/clippy/rust-analyzer) is provisioned via `../shell.nix` + `.envrc` (direnv/nix).

## Architecture

`Client` (`src/lib.rs`) is the SDK entry point: `Client::connect(documents_url)` opens a gRPC connection and exposes service-specific sub-clients (currently `documents()` → `&mut DocumentsClient`). As more server services are added, they should follow the same pattern: a submodule under `src/`, wired into `Client`.

### Documents service (`src/documents/`)

Layered in three pieces, each only aware of the layer below it, following the same `mod.rs`/`service.rs` split as `../server`'s domain modules:

- `mod.rs` — public domain types (`Document`, `NewDocument`) and the `Service` trait (`save`/`upload`/`fetch`), the business-level API other code depends on. Trait methods take `&self` (not `&mut self`) so implementations can be shared behind `Arc<dyn Service>`.
- `grpc_client.rs` — thin gRPC transport (`GrpcClient`). Knows nothing about documents semantics; just sends proto requests and returns raw proto responses. Methods take `&self`, cloning the underlying tonic `Channel` per call (cheap, safe to use concurrently) since the generated client's RPC methods require `&mut self`. Generated protobuf bindings are compiled from `../api-protos/documents.proto` by `build.rs` (via `tonic-prost-build`) and included with `tonic::include_proto!("documents")`.
- `crypto.rs` — envelope encryption primitives (AES-256-GCM). Generates a fresh per-document data encryption key (DEK), encrypts content and metadata under it, and wraps the DEK under a key-encryption-key (KEK) before anything leaves the process. The server only ever stores/returns ciphertext and a wrapped DEK — it cannot read document content or metadata (name, content type, etc.).
- `service.rs` — `DocumentsClient`, the default `Service` implementation, which composes the crypto and gRPC layers and delegates local, unencrypted persistence to an injected `Storage`. `fetch` is a single round-trip: the server's `FetchDocument` RPC returns an `EncryptedDocument` (ciphertext content, wrapped DEK, and encrypted metadata together), so no separate lookup is needed to decrypt it. There is no listing RPC — `ListDocuments` was removed from `documents.proto`.

**Known temporary state**: `crypto.rs` derives its KEK from a hard-coded placeholder secret (`TEMP_HARDCODED_KEK_SECRET`), explicitly marked in a doc comment as needing replacement with a real KMS/HSM/secrets-manager-sourced key before handling real data. Don't remove that comment when touching this file unless the underlying issue is actually fixed.

### SQLite client (`src/sqlite.rs`)

`SqliteClient` manages a local on-disk database independent of the gRPC services, at `$XDG_DATA_HOME/fyde/fyde.db` (falling back to `~/.local/share/fyde/fyde.db`), created on first connect. Uses a single-connection pool deliberately, since SQLite only supports one writer at a time.

### Errors

All fallible SDK operations return the crate-wide `Result<T> = Result<T, Error>` (`src/lib.rs`). `Error` is a single enum covering every failure domain (gRPC transport/status, UUID parsing, MessagePack, I/O, sqlx, XDG, JSON, encryption) via `thiserror` `#[from]` conversions — add new variants there rather than introducing per-module error types.
