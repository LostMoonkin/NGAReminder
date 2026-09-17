# NGA Reminder

[中文](README.md)

NGA Reminder monitors NGA threads and users, saves content, and sends notifications. The Go rewrite has completed its runtime foundation, admin login, SQLite storage, and request tracing. Business features will follow in subsequent phases.

## Repository

| Directory | Status | Purpose |
| --- | --- | --- |
| [`service/`](service/docs/README.md) | Phase 01 complete | Single-process Go server using Gin, GORM, pure Go SQLite, and zerolog, with CGO disabled |
| [`extension-standalone/`](extension-standalone/) | Maintained independently | Browser-only Chromium extension using browser cookies |
| [`archive/rust-service/`](archive/rust-service/ARCHIVE.md) | Historical archive | Rust v0.1.4 source, tests, migrations, deployment files, project plan, and design documents |

Start with the [server guidelines](service/AGENTS.md) and [rewrite plan and phased specs](service/docs/plan/README.md), maintained in Chinese. Server documentation lives under `service/docs/`, divided into `plan/`, `spec/`, and `ticket/`. The rewrite retains the features in use and removes runtime PostgreSQL and multi-instance support. Existing deployments stop Rust, migrate PostgreSQL data to SQLite, validate the result, then bring Go online; see the [data migration spec](service/docs/spec/09-data-migration.md). Implementation details remain with each phase; tickets have not been created.

See the [local startup instructions](service/docs/README.md#本地运行). Go 1.26 or newer is required; builds and tests use `CGO_ENABLED=0`.

## Rust archive

The archive is based on commit `7a9c9dd3c7fa0d33cdadcf6829f2c8f7d0ca0d63` and includes the subsequent service review.

- [Archive index and original paths](archive/rust-service/ARCHIVE.md)
- [Original project plan](archive/rust-service/PROJECT_PLAN.md)
- [Original server documentation](archive/rust-service/service/README.en.md)
- [Service complexity review](archive/rust-service/service/docs/SERVICE_SIMPLIFICATION_REVIEW.md)

Archived designs, milestones, and Rust conventions describe the historical implementation. They are reference material for the Go design. The Rust image workflow has been moved out of the active workflows directory, and server releases are paused.

## Standalone extension

The extension retains its independent runtime, storage, and release version. See the [extension documentation](extension-standalone/README.md).

```bash
scripts/release.sh extension <x.y.z>
```

The script creates a local commit and a `vX.Y.Z-standalone` tag. Add `--push` explicitly to publish them. Checks and packaging remain in the [extension workflow](.github/workflows/extension-release.yml).

## License

[MIT](LICENSE)
