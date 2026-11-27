# Project notes

## Build & release workflow
- The user's phone cannot run heavy builds: write/edit code here, but do NOT run `cargo build`, `cargo build --release`, `package_deb.sh`, or other heavy local compilation.
- Push the code changes to GitHub; GitHub workers build the `.deb` and publish it. The user installs the resulting package on their machine.

## Packaging
- `package_deb.sh` expects a release binary at `/workspaces/Link-code/link-code/target/release/link-code`, hardcodes `VERSION=1.0.30`, and writes the `.deb` to `/tmp/opencode/`. Bump the version when cutting a release.
- No `.github/workflows` directory exists in this checkout; deb automation must be configured on the GitHub side (or added explicitly if the user asks).

## Sandbox
- The `git` CLI is not installed in the sandbox; use the built-in git tools for status and commits.
