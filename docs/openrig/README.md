# Running jcode seats in OpenRig

[OpenRig](https://openrig.dev) is a local control plane for teams of agents:
rigs, seats, queues, handover, restore. jcode can sit in any OpenRig seat as a
**community harness**. Each seat is one thin jcode TUI client. All seats share
one jcode daemon, so a large fleet costs one server's worth of memory plus a
small client per pane, instead of one full agent runtime per seat.

## Install

```bash
mkdir -p ~/.openrig/community-harnesses
cp docs/openrig/jcode.yaml ~/.openrig/community-harnesses/jcode.yaml
rig community-harness list
```

Then use `runtime: jcode` for a member in a rig spec, or
`rig create <name> --runtime jcode`.

`jcode-lead.yaml` is a role variant of the same harness: medium reasoning
effort with Fast (provider priority tier) enabled, for lead seats. Use
`runtime: jcode-lead` for those members. For more variants (one per model or
role), copy the file, set a unique `name:`, put
`JCODE_OPENRIG_HARNESS=<name>` before the command so the launch record names
the right preset, and adjust the flags. OpenRig rejects a launch record whose
harness name differs from the preset.

## Per-seat model, effort, and speed

- **Model**: declare it on the rig-spec member (OpenRig passes it via
  `OPENRIG_MODEL`; the preset sets `passes_model: true`).
- **Reasoning effort**: `--effort <level>` on the preset command (for
  example `low`, `medium`, `high`). Applied once the session attaches,
  exactly like typing `/effort <level>`.
- **Fast mode**: `--fast on|off` on the preset command, exactly like
  `/fast on|off` (OpenAI priority service tier).

Both flags are per-session requests, so they work against the shared daemon
without restarting it, and they survive seat relaunch/restore because they
live in the preset command.

## Protocol

jcode reads OpenRig's launch variables directly. The preset command is plain
`jcode`.

| Variable | jcode behavior |
| --- | --- |
| `OPENRIG_LAUNCH_RESULT`, `OPENRIG_LAUNCH_ATTEMPT` | Once the server assigns the session, the client atomically writes `{format:1, attempt, harness, nativeRef:<session id>, producer:"jcode"}`. |
| `OPENRIG_RESUME_REF` | Same as `--resume <id>`. If the session no longer exists, jcode writes `error:"missing"` and exits without starting anything. |
| `OPENRIG_MODEL` | Same as `--model`, unless `--model` is given. |
| `OPENRIG_FIRST_INPUT` | The file's text is sent as the first prompt. |

jcode removes these variables from its own environment at startup. The daemon
it may spawn, its bash tool, and nested jcode processes never inherit them, so
they cannot overwrite the seat's identity.

## Activity reporting (runner contract)

The presets declare `runner: true`: jcode is its own OpenRig runner and posts
turn boundaries to `POST <OPENRIG_URL>/api/activity/hooks` per OpenRig's
harness-runner contract, so seat activity reads **working/idle** from hooks
instead of "unknown" from window sampling:

| Moment | Post |
| --- | --- |
| Message submitted | `UserPromptSubmit` / `accepted` with a rising `turnId` |
| Streaming text / tool starts | `active` / `agent_message`, `PreToolUse` / `<tool>` (throttled) |
| Turn finished | `Stop` / `end_turn` (or `cancelled` after an interrupt) |
| Client exits | `Stop` / `<runtime>_exited`, so a dead seat never reads idle |

Every post carries `sessionName`, `nodeId`, `generation`, a process-wide
rising `sequence`, and `occurredAt`. The hook token
(`OPENRIG_ACTIVITY_HOOK_TOKEN`, plus `OPENRIG_RUN_ID` for run-bound grants)
is captured at startup and scrubbed from the environment, so child processes
never see it; an in-pane reload/update re-exports it to the successor jcode.
Posting is best-effort on a background thread with short timeouts and never
blocks the agent.

## Limits

- OpenRig places no skills or startup files into a community-harness seat.
  jcode uses `AGENTS.md` and its own skills from the working directory.
- Fork is unsupported. Resume reopens the same session.
