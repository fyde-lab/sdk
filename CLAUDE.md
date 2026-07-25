# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
# Build
cargo build

# Run all tests (unit + integration)
cargo test --all --verbose --locked

# Run a single test by name
cargo test <test_name>

# Lint (warnings are errors in CI)
cargo clippy --all-features --all-targets --locked -- -D warnings

# Format check
cargo fmt --all -- --check

# Auto-format
cargo fmt --all
```

## Architecture

This is a Rust library crate (`sdk`) that provides document management with SQLite persistence and PDF processing.

**Entry point:** `Sdk::init(cfg)` in `src/lib.rs` — accepts `StorageType::Memory` or `StorageType::FileSystem` (XDG-based path), initializes the SQLite connection, runs migrations, and returns an `Sdk` struct with service handles.

**Layer structure (per domain module):**

```
src/documents/
  mod.rs      — public types (Document, Metadata), wires storage → service
  service.rs  — Service trait + Svc impl (business logic: file validation, PDF rendering/text extraction)
  storage.rs  — Storage trait + SqliteStorage impl (SQL via sea-query)
src/storage.rs — shared SQLite setup (WAL mode, migrations via rusqlite_migration)
src/errors.rs  — top-level Sdk init errors
```

**Key patterns:**
- Both `Service` and `Storage` traits are defined with `#[automock]` (mockall), so unit tests in `service.rs` mock the storage layer directly — no real DB needed for unit tests.
- The integration test in `tests/integration_test.rs` uses `StorageType::Memory` and exercises the full stack end-to-end.
- SQLite UUID storage: UUIDs are stored as raw bytes (`BLOB`), not strings. Queries filter using `.as_bytes().as_ref()`.
- `Metadata` and `Document` structs use `#[readonly::make]` — fields are public but only settable within the module.
- Migrations live in `migrations/` as numbered directories (`001-create-document-table/up.sql`) and are embedded at compile time via `include_dir!`.

**PDF processing:** `save_file_from_path` validates MIME type (only `application/pdf` allowed), renders the first page to PNG via `hayro`/`vello_cpu`, and extracts text via `lopdf`. Both the raw bytes and rendered preview are stored in SQLite.

## Definition of done

A task is not complete until `cargo test --all --verbose --locked` passes with no failures.

## Commit style

Uses Commitizen with conventional commits (`feat`, `fix`, `refactor`, `ci`, etc.). Scope is the module name (e.g., `feat(document): ...`).
