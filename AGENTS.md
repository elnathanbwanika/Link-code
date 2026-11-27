# Project notes

## Build & release workflow
- The user's phone cannot run heavy builds: write/edit code here, but do NOT run `cargo build`, `cargo build --release`, `package_deb.sh`, or other heavy local compilation.
- Push the code changes to GitHub; GitHub workers build the `.deb` and publish it. The user installs the resulting package on their machine.

## Packaging
- `package_deb.sh` defaults to a release binary at `/workspaces/Link-code/link-code/target/release/link-code`, `VERSION=1.0.36`, and `/tmp/opencode/`; override with `BINARY`, `VERSION` and `OUT_DIR`.
- `.github/workflows/build-deb.yml` builds on `ubuntu-24.04` for every push, uploads the `.deb` as an artifact, and attaches it to a GitHub release for `v*` tags.

## Model adapters
- Shared tool schemas are OpenAI-shaped and include `additionalProperties`; the Gemini adapter strips it recursively because Google rejects that field in function declarations.
- Gemini 4/3 require `thoughtSignature` on replayed `functionCall` parts; it is stored on `ToolCallRequest` and restored sessions without one use Google's documented `skip_thought_signature_validator` dummy.
- `RotatingProvider` retries a whole key rotation up to four times for rate limits/transient 5xx with capped backoff; `RetryObserver` toasts the wait and Stop interrupts the pre-stream sleep.
- OpenAI-compatible tool-only assistant turns send `content: null`; the Tools switch must stay on for file/shell tools.

## Conversation continuity
- Sessions persist tool calls (with Gemini signatures) and tool results; `Session::chat_history()` rebuilds the exact sequence for the next model.
- Changing the chat model or reopening a session sets a one-shot handoff prompt; ordinary turns get no "continue this session" instruction.

## Layout
- Shared panel minimums live in `link-ui/src/window.rs` (`SIDEBAR_MIN`, `BOTTOM_MIN_HEIGHT`); panels import them instead of duplicating requests.
- One control per action: Settings lives in the header, the model picker and mode switch in the composer; the status bar is read-only.

## Sandbox
- The `git` CLI is not installed in the sandbox; use the built-in git tools for status and commits.
