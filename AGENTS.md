# Project notes

## Build & release workflow
- The user's phone cannot run heavy builds: write/edit code here, but do NOT run `cargo build`, `cargo build --release`, `package_deb.sh`, or other heavy local compilation.
- Push the code changes to GitHub; GitHub workers build the `.deb` and publish it. The user installs the resulting package on their machine.

## Packaging
- `package_deb.sh` defaults to a release binary at `/workspaces/Link-code/link-code/target/release/link-code`, `VERSION=1.0.30`, and `/tmp/opencode/`; override with `BINARY`, `VERSION` and `OUT_DIR`.
- `.github/workflows/build-deb.yml` builds on `ubuntu-24.04` for every push, uploads the `.deb` as an artifact, and attaches it to a GitHub release for `v*` tags.

## Sandbox
- The `git` CLI is not installed in the sandbox; use the built-in git tools for status and commits.
