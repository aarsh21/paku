# Paku verification

Paku is an independent Pi-only fork of [Zeron](https://github.com/zeronsh/zeron). Production discovery registers only Pi. Mock remains an explicitly injected test double; Pi's underlying model providers are retained.

## Reproduce

Install the repository Rust toolchain, Node, Python 3, the platform build dependencies in `dist/README.md`, and Pi. On Linux, native UI evidence also needs Xvfb, xdotool, FFmpeg and ImageMagick. Install `agent-browser` for browser-level landing checks.

```sh
npm --prefix edge ci
scripts/verify-paku.sh /tmp/paku-evidence
(cd /tmp/paku-evidence && sha256sum -c SHA256SUMS)
```

The script selects only the safe ignored Pi tests; do not run every ignored workspace test indiscriminately. Some intentionally use real model quota or privately supplied incident snapshots.

## What the evidence establishes

- Workspace tests exercise the engine, client, protocol, queues, sessions, account storage, MCP, document persistence, UI, diffs, terminals, sync and other retained crates. The native-fixture feature is enabled explicitly. All-feature/all-target compilation also checks the optional UI fixtures.
- Genuine Pi lifecycle tests run the installed CLI against an isolated local provider extension, including errors, interruptions, extension commands and steering. They do not use an imitation Pi executable or a paid provider API.
- The full-application IPC test launches the real `paku headless` executable. It creates a workspace/chat over WebSocket RPC, observes live transcript updates, reads persisted output, restarts the application, and asserts the same native Pi session UUID resumes. It rejects transcript error parts, including MCP startup failures.
- Native Linux evidence comes from a private Xvfb display. Actual keyboard input enters the production desktop composer and produces a reply through the engine and genuine Pi process. Each run has fresh data and a unique input nonce. PNGs, MP4 and raw RPC events are saved; the visible `MOCK:` response is deliberately supplied by Pi's local model fixture.
- Two-device sync runs real headless engines and a local Wrangler/workerd Worker with synthetic dev-auth identities. B queues work to A, A executes the **Mock harness**, and transcript/session state converge back to B. Free ports, temporary engine data and private Worker persistence avoid touching a user's existing workspace.
- Landing/redirect and edge tests cover the independent product/service defaults, attribution, schemas and local Durable Object behavior. Browser checks verify desktop/mobile layout and source-build navigation.
- Swift is regenerated from the current Rust UniFFI metadata and compared byte-for-byte with the checked-in binding. This checks binding consistency, **not an iOS runtime**.
- IPC regressions reject a compatible upstream daemon without the Paku product marker. Local-only auth regressions prevent a synthetic dev login when no edge is configured. Git diff regressions use default worker stack sizes and explicitly override personal diff-prefix settings.

## Recorded Linux run (2026-10-06)

Evidence directory on the verification host: `/tmp/paku-evidence-final`.

| Check | Result |
| --- | --- |
| Workspace, all eligible targets | **2,880 passed, 0 failed**, 16 opt-in cases ignored; 100 test binaries |
| All features / all targets | Compilation passed |
| Genuine Pi 1.0.4 | Two lifecycle/steering tests and one full-application IPC/resume test passed |
| Native desktop | Keyboard input and rendered local-fixture reply; PNG, MP4 and RPC JSON saved |
| Cross-device sync | Real engines + local Worker, explicit Mock harness; passed |
| Web/edge | 6 landing/redirect, 61 edge-unit and 25 workerd tests passed; typecheck and browser checks passed |
| Swift / installer | Generated binding matched; Linux installer/desktop-entry checks passed |

All **22** recorded verification suites exited zero. Three of the root suite's ignored cases are the safe genuine-Pi cases executed explicitly afterward. The remaining ignored cases are visible with reasons in `rust.log`. The recorded tested binary's SHA-256 was also checked against `target/debug/paku` after the complete run.

## Artifacts

`results.tsv` records each command's exit code and log. `.command` files record invocations. `environment.txt` identifies the platform and tool versions. The evidence includes `pi-engine-ipc.json`, native UI JSON/PNGs/video, browser snapshots, and raw sync engine/Worker logs.

`source-sha256.txt` covers the tracked and untracked source, including root manifests, lockfile, documentation and workflows. `changes.patch` plus `untracked-files.tar.gz` reconstruct the working tree at `source-metadata.json`'s base revision. Ignored credentials/build caches are excluded. `SHA256SUMS` binds the saved artifacts. Source or artifact changes after collection require running `scripts/collect-paku-evidence.py OUTPUT` again.

These are inspectable and reproducible records, not a cryptographic attestation of the host or a guarantee that every possible user input works. Historical results under `docs/performance/` belong to Zeron and are not Paku validation.

## Limits

The current verification host is Linux; Pi is tested with a local fixture. Paid model-provider APIs/OAuth, interactive self-hosted WorkOS sign-in, production Cloudflare deployment, macOS/Windows runtime, physical iOS/device execution, microphone capture, and the optional native embedded-browser interaction fixture are not certified by this run. Opt-in private-data, paid-model, live-edge and diagnostic benchmark cases remain separately identified as ignored; the safe Pi cases are run explicitly afterward. The local sync smoke test is distinct from those opt-in integration cases.
