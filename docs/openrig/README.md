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

For more presets (one per model, say), copy the file to
`jcode-opus.yaml`, set `name: jcode-opus`, and add
`JCODE_OPENRIG_HARNESS=jcode-opus` before the command via a wrapper script.
OpenRig rejects a launch record whose harness name differs from the preset.

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

## Limits

- OpenRig places no skills or startup files into a community-harness seat.
  jcode uses `AGENTS.md` and its own skills from the working directory.
- Fork is unsupported. Resume reopens the same session.
- Activity is detected from the pane, not from hooks.
