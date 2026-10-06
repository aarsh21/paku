# Paku MCP server

`paku mcp` serves newline-delimited JSON-RPC MCP on stdin/stdout and proxies tools into the running engine's localhost IPC (`ws://127.0.0.1:$PAKU_IPC_PORT`, default 28654). It is a subcommand of the same desktop/headless binary. The protocol layer supports `initialize`, `ping`, `tools/list` and `tools/call` without a separate Node server.

## Identity and Pi injection

The engine passes `PAKU_IPC_PORT`, `PAKU_CHAT_ID` and `PAKU_DEVICE_ID` to its MCP child. Chat identity adds a routing header to agent-to-agent messages and prevents a chat from messaging itself.

Pi's native RPC process loads a temporary per-run `--extension` that bridges stdio MCP into Pi tools. The extension registers app-prefixed tools, propagates cancellation/errors and closes the MCP child when the Pi session shuts down. User Pi settings, extensions and session arguments remain intact. Title-only runs do not carry the bridge. An embedded engine that lost its IPC bind does not advertise a dead server. Paku does not install a separate Pi subagent extension.

## Tools and host selection

| Tool | Purpose |
| --- | --- |
| `whoami` | Local device, engine and origin chat identity |
| `list_devices`, `list_projects` | Discover execution hosts and workspaces |
| `list_harnesses`, `list_models` | Discover the selected host's Pi availability and models |
| `list_chats`, `get_chat`, `read_chat` | Registry status and transcript snapshots |
| `create_chat`, `create_chats` | Create a standalone or child conversation, optionally send a prompt |
| `send_message`, `send_messages` | Send/steer or explicitly queue work |
| `wait_for_turn` | Wait for new completion/input/error state |
| `interrupt_chat`, `respond_to_input` | Interrupt work or answer a pending question |
| `archive_chat` | Archive a conversation |

IDs are preferable to ambiguous names. `device` selects a host; `project` determines its owning host. A project from another device is rejected. Without either, a conversation is projectless on the local engine. Discovery/model validation uses the target host's catalog, not a fallback local catalog. Pi must be available on that host.

```json
{"kind":"chat", "device":"<device-id>", "project":"<project-id>", "harness":"pi", "model":"<host catalog model-id>", "title":"Implement feature", "prompt":"Implement the feature", "wait":false}
```

`kind: "chat"` is standalone. `kind: "side"` requires an explicit parent or origin chat. Omitted kind retains the origin-parent default, or standalone when there is no parent. Contradictory/unknown kinds fail before writes. Child chats cannot create further chats; one level is supported. Children remain accessible through MCP and the explorer even when the primary sidebar hides them. Forks copy completed history and record provenance.

## Steering and waiting

`send_message` mode `auto` starts an idle chat or steers active Pi work at a native input boundary without interrupting tools. `queue` creates a held queue row. A pending input request must use `respond_to_input`. See [Pi lifecycle semantics](pi.md): acknowledgment is not the same as incorporation or completion.

A wait following a send uses the session baseline captured before dispatch, including brand-new chats with no session row yet. It waits for a new completion/input/error state rather than returning a previous response. Completion and transcript arrival share one timeout; missing new response data produces `timedOut`. A separate MCP connection without the send baseline reports current posture.

## Parallel child work

`create_chats` and `send_messages` accept 1–32 independent requests and return results in input order with per-item success/error. Partial failure does not roll back other requests. Defaults are nonblocking. Do not batch ordered messages to the same chat.

```json
{"requests":[{"project":"/repo", "title":"Review tests", "prompt":"Review test coverage"},{"project":"/repo", "title":"Review API", "prompt":"Review API compatibility"}]}
```

For individual tools, dispatch all independent work with `wait:false` before collecting replies with `wait_for_turn`.

## Validation

```sh
cargo test -p paku-mcp
cargo test -p paku-harness --features native-fixture --test pi_mcp
```

These deterministic suites exercise dispatch and the Pi bridge. Live model behavior and optional multi-device deployment require separate validation; no hosted Paku backend is provided.
