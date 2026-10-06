# Context usage

The conversation composer includes a context ring for local, projectless and remote chats. Hovering shows measured tokens and remaining capacity when the host has sufficient data. Amber starts at 75%, red at 90%; drawing clamps to a full circle while the label preserves over-capacity values. A dash means unavailable, not zero. A measured zero is shown as 0%.

Pi is the only production harness. The host normalizes native context occupancy independently of billing totals; it must not present cumulative cost/token accounting as the current context size. Mock and older hosts may report unavailable. Availability depends on the installed Pi version and native state reported by the integration.

`AgentEvent::ContextUsage` updates atomic `meta.contextUsage` state. Partial values preserve known fields and zero capacity is ignored. New non-resumed runs clear old values. Post-turn snapshots can refresh the meter without reopening a completed turn; child activity must not overwrite the parent's meter.

Document storage/sync and typed `TranscriptUpdate` envelopes carry the snapshot, including context-only commits and opening resets. UI rendering uses cached state rather than polling providers or accessing files. Historical screenshots are inherited deterministic fixtures, not evidence of a Paku release or live model measurement.
