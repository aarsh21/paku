# Pi commands and skill completion

Paku's `/` picker combines workspace actions with Pi's native `get_commands` catalog (including extension commands and `skill:*` invocations). Pi is the only production harness. Install and configure skills/extensions through Pi; Paku does not install other agent adapters.

## Workspace actions

| Command | Action |
| --- | --- |
| `/model` | Choose a Pi model and thinking level |
| `/new` | Start a conversation |
| `/resume` | Search/open conversations |
| `/settings` | Open app settings |
| `/diff` | Open changes |
| `/files` | Open project files |
| `/terminal` | Open a terminal |
| `/rename` | Rename the current conversation |
| `/stop` | Interrupt the current run |

Workspace actions run locally without creating a model turn. Conversation-dependent actions appear only in an existing chat. When a native Pi command owns the same name, the workspace action is namespaced with `paku:`. Selecting an action removes its trigger while preserving surrounding draft text, attachments and queued edits. Literal code, URLs, paths and unselected inline `/word` text are not executed as local actions.

## Skills and delivery

Composer completion preferences control `$` skill completion and whether skills are separated from the `/` menu. Enabling `$` alone makes skills available in both menus. Selecting a skill retains its identity; an arbitrary dollar sign is not a skill reference.

Discovery runs on the execution host in the project's directory. Pi's native skill namespace identifies advertised skills; file discovery also considers standard project/user and shared `.agents/skills` directories. Missing or unreadable unrelated roots are skipped with diagnostics. This is not a complete parser of every custom plugin configuration.

Native commands and skills are delivered on Send through Pi's protocol and leading-command rules. Hosts advertise `composer-references-v1` for canonical reference delivery. An unsupported host preserves the draft and asks for an update rather than dropping file/command/skill chips.

See [Pi integration](pi.md) and [Pi skills](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/skills.md).
