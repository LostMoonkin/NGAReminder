# NGA Reminder

[中文](README.md)

NGA Reminder monitors NGA threads and users, saves content, and sends notifications. The server is being prepared for a Go rewrite. The Rust implementation has been archived; detailed Go design is still pending.

## Repository

| Directory | Status | Purpose |
| --- | --- | --- |
| [`extension-standalone/`](extension-standalone/) | Maintained independently | Browser-only Chromium extension using browser cookies |
| [`archive/rust-service/`](archive/rust-service/ARCHIVE.md) | Historical archive | Rust v0.1.4 source, tests, migrations, deployment files, project plan, and design documents |

The Go server's structure, technical design, and implementation plan will be decided separately.

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
