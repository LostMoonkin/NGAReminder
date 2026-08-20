# Repository Guidance

## Start Here

1. Identify the product in scope before editing: the Rust service and the standalone Chromium extension are independent products with separate storage, runtime, versioning, and release flows.
2. Read the task-specific context below before designing a change. Treat accepted ADRs and focused design documents as the decision record; verify current implementation details in code and tests.
3. Make the smallest coherent change across code, persistence, API/UI, documentation, and tests. A behavior change is complete only when every affected external contract is accounted for.

## Context Routing

- For monitoring, scheduling, runs, cursors, baselines, or notification terminology, read `CONTEXT.md` and use its canonical language.
- For no-fetch periods, manual/automatic run provenance, or shared run coordination, read `docs/adr/0001-separate-no-fetch-periods-from-fetch-schedules.md` and `service/docs/NO_FETCH_PERIODS_DESIGN.md`.
- For watch configuration, initialization modes, notification matching, event creation, and outbox ownership, read `service/docs/MONITORING_NOTIFICATION_REDESIGN.md`.
- For NGA request shapes, response codes, authentication behavior, or parser fixtures, read `service/docs/NGA_API_CONTRACT.md` and `service/tests/fixtures/nga/README.md`.
- For bot commands, bindings, authorization, or Cookie renewal, read `service/docs/BOT_INTERACTION_AND_COOKIE_RENEWAL_DESIGN.md`.
- For exports, assets, cleanup, or content rendering, read `service/docs/EXPORT_RESOURCE_AND_CONTENT_UI_DESIGN.md`.
- For deployment, backup, restore, migration, or production configuration, read `service/docs/OPERATIONS.md`.
- For standalone extension behavior, read `extension-standalone/README.md` and its release workflow. Apply service behavior to the extension only when the task explicitly includes both products.

## Rust Service Conventions

- Work from `service/`. The crate uses Rust 2024 and supports SQLite and PostgreSQL through `sqlx::Any`.
- Add a new, equally numbered migration under both `service/migrations/sqlite/` and `service/migrations/postgres/` for schema changes. Keep their observable schema and constraints equivalent; retain backend-specific timestamp syntax only where required.
- Preserve transaction boundaries around cursor movement, baseline completion, event matching, outbox enqueueing, and run finalization. A failed or skipped collection must not silently advance a cursor or establish a baseline unless the relevant design explicitly says so.
- Treat leases and persistent job state as recovery contracts. Claim, renew, finish, and invalidate paths must remain safe across process crashes and duplicate worker cycles.
- Keep TID and UID collection semantics distinct inside their collectors, while placing shared scheduling, run bookkeeping, and trigger behavior above them.
- Keep public API changes synchronized with request validation, response types, `/admin`, README examples, and focused design documentation.
- Use fixture-backed or local database tests for NGA behavior. Live NGA access is a manual diagnostic, not an automated test dependency.
- Keep credentials and recipient targets encrypted at rest and redacted from responses, errors, logs, fixtures, and snapshots.

## Standalone Extension Conventions

- Treat `extension-standalone/` as a browser-only application with Chrome local storage and no server database or persistent run ledger.
- Keep Manifest V3 service-worker constraints in mind. Validate JavaScript syntax and static behavior with the commands defined in `.github/workflows/extension-release.yml`.
- Preserve the extension's independent version in `manifest.json`; service and extension releases use different tags and may ship independently.

## Test Seams

- Prefer the highest stable seam that exposes the behavior. For service workflows, test through the shared coordinator or HTTP API instead of duplicating assertions in both collectors.
- Test pure time, schedule, parser, and formatting rules as deterministic functions with explicit timestamps and time-zone offsets.
- Use repository/API integration tests to cover transactions, leases, status transitions, idempotency, SQLite behavior, and serialized contracts.
- Add collector-specific tests only for behavior unique to TID or UID acquisition and cursor semantics.
- Assert externally meaningful state: API responses, persisted rows, cursor/baseline values, run records, events, outbox entries, and metrics. Avoid assertions tied only to private call order.

## Validation

For service changes, run the same quality gates as CI from the repository root:

```bash
cargo fmt --manifest-path service/Cargo.toml --all -- --check
cargo test --manifest-path service/Cargo.toml --locked --all-targets
cargo clippy --manifest-path service/Cargo.toml --locked --all-targets --all-features -- -D warnings
```

For extension changes, run every validation step in `.github/workflows/extension-release.yml`. For documentation-only changes, run `git diff --check` and verify every relative link resolves.

When a relevant validation cannot run locally, report the exact command and blocker. Completion means the scoped checks pass, both database backends remain represented where persistence changed, public documentation matches behavior, and no sensitive value appears in the diff.
