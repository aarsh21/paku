# Paku architecture

Paku is a Pi-only fork of [Zeron](https://github.com/zeronsh/zeron). The original native application and sync architecture were built by the Zeron contributors. The fork narrows the production harness surface to Pi; it does not replace that provenance with a claim of original authorship.

## Local-first topology

```text
GPUI desktop ── typed RPC ── Rust engine ── Pi native RPC subprocess
                                  │
                                  ├── local documents, journals, uploads
                                  ├── repositories, worktrees, terminals
                                  └── optional self-hosted edge sync
```

`cargo run -p paku` opens the desktop UI. It attaches to an existing local daemon when available, otherwise hosts an in-process engine. `cargo run -p paku -- headless` runs the engine without a window. Both use the same typed request/response/event boundary. Tokio handles engine I/O; GPUI renders the desktop without blocking on subprocess or network work.

No hosted Paku service is supplied. See [self-hosting](docs/self-hosting.md) before enabling cloud transports. Endpoint overrides are deployment seams, not a promise of a stable third-party hosting API.

## Workspace responsibilities

| Path | Responsibility |
| --- | --- |
| `crates/proto` | Wire entities, agent events, RPC envelopes, shared view derivations |
| `crates/harness` | Pi discovery, `pi --mode rpc`, native sessions, steering, extension dialogs; deterministic mock fixtures for tests |
| `crates/engine` | Session lifecycle, command execution, journals/recovery, workspace files, git/worktrees, terminals, auth and device routing |
| `crates/doc` | Loro session/registry schemas, incremental mirrors, parts and command ledgers |
| `crates/sync` | Local snapshot storage and optional room transport |
| `crates/rpc` | Localhost/in-memory RPC and optional device-room virtual sockets |
| `crates/ui` | GPUI shell, transcript, composer, files, diff, terminal, previews and settings |
| `crates/theme` | Validated light/dark theme families, import/link compilation, last-known-good palettes |
| `crates/voice` | Optional desktop-local dictation capture and inference |
| `crates/preview` | Preview networking and browser integration |
| `crates/update` | Maintainer-configured release/update machinery |
| `apps/paku` | Desktop/headless binary and CLI |
| `edge` | Optional TypeScript Cloudflare Worker, Durable Objects and R2 |
| `apps/landing` | Static source-build information, without release or hosting claims |

Pi is the only production harness. Model providers, authentication and model catalogs belong to Pi. The mock harness is a test fixture, not another supported agent integration. [Pi integration](docs/pi.md) describes native session files, extensions and lifecycle semantics.

## State and privacy boundaries

Authentication is separate from the workspace profile captured at engine startup. Changing credentials does not silently replace an open database. Local sessions and attachments remain in the local profile; signing in does not automatically publish them. Switching local/synced profiles requires an engine restart.

Session documents hold transcripts and a durable command queue. The host device executes commands, with processed-command tracking for deduplication. Tool rendering omits private payloads where appropriate; full run inputs can remain in host-local journals. The workspace registry holds indexed spaces, chats and status rather than every transcript. Local snapshots work without a room connection.

Synced devices on one authenticated account are **trusted peers**. Remote workspace requests are subject to path containment, symlink and conflict checks, but ignored files such as `.env` are not an authorization boundary. Do not enroll devices that should not read or write that workspace. `.git` remains unavailable through workspace file APIs.

## Optional edge

The worker verifies authentication before forwarding to private rooms or R2. Session and registry updates use row/log/checkpoint protocols; device rooms relay control traffic and nudges; preview rooms support browser transport. WorkOS auth exchange and organization routes are retained for deployments that configure their own tenant. APNs requires the operator's own key, key ID, team ID and topic.

Checked-in configs use fresh Paku resource names, no custom domain routes, no Cloudflare account ID and no WorkOS client credentials. Production authentication fails closed until explicitly configured. Development bearer authentication must stay local and must never be enabled on a public deployment. Durable Object storage is associated with worker identity: these templates are new deployments, not migration tools for upstream resources.

## UI and validation

The desktop uses virtualized transcript rows, incremental markdown, paint-only theme palettes and async syntax highlighting. Files, diffs, terminals, queues, attachments, previews and local theme management are retained independently of the Pi harness choice. Historical UI/performance notes under `docs/` describe inherited implementation work, not new Paku release evidence.

- `node --test apps/landing/*.test.mjs`: landing honesty and link contracts.
- `npm --prefix edge run typecheck`, `npm --prefix edge test`, `npm --prefix edge run test:workerd`: edge unit and real Worker runtime tests.
- `scripts/ci/test-core.sh`: deterministic Rust/Pi fixture suites through nextest.
- `scripts/e2e-smoke.sh`: two headless engines and local edge, using the mock harness to prove cross-device commands and transcript sync.
- `cargo test -p paku-harness --test pi_live -- --ignored --nocapture`: installed Pi with isolated settings and a local provider. This proves native Pi protocol behavior, not remote model quality.

A test command listed here is a reproducible validation path, not a claim that every platform or cloud deployment has been validated in this fork.
