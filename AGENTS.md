# Repository Guidance

## Product Scope

- The server is planned for a Go rewrite. Detailed design and implementation have not started; establish the new design from the current task rather than treating the Rust archive as the Go specification.
- `extension-standalone/` remains an independent Chromium application with separate storage, runtime, versioning, and releases.
- `archive/rust-service/` preserves Rust v0.1.4 and its historical project plan, domain context, ADRs, source, tests, and release configuration.

## Context Routing

- For historical server behavior, protocol fixtures, or the complexity review, start at [the archive index](archive/rust-service/ARCHIVE.md). Follow its focused references as needed.
- For work explicitly targeting archived Rust code, follow [the archive guidance](archive/rust-service/AGENTS.md).
- For extension behavior, read [its README](extension-standalone/README.md) and [release workflow](.github/workflows/extension-release.yml). Apply server behavior to the extension only when the task includes both products.

## Standalone Extension

- Use Chrome local storage and respect Manifest V3 service-worker constraints.
- Preserve the extension's independent version in `manifest.json` and its `vX.Y.Z-standalone` tags.
- Run the validation steps in the extension release workflow for extension changes.

## Validation

- For archival or documentation changes, verify that moved files are preserved, relative links resolve, and `git diff --check` passes.
- Keep archived workflows outside the root `.github/workflows/` directory. The active release script currently supports only the extension.
- Keep local credentials, databases, downloaded assets, and build output out of version control. Preserved local Rust state is under the ignored `.local/rust-service/` directory.
