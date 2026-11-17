# Lifecycle Hooks

jcode can run external commands at well-defined lifecycle points so other
programs can observe or gate agent behavior without forking jcode. Hooks
complement the [spawn hook](SPAWN_HOOK.md) (which controls *where headed
sessions appear*); lifecycle hooks tell you *what is happening inside them*.

## Configuration

```toml
# ~/.jcode/config.toml
[hooks]
turn_start    = "~/bin/jcode-turn-busy"       # observer
turn_end      = "~/bin/jcode-turn-notify"     # observer
session_start = ""                            # observer
session_end   = ""                            # observer
pre_tool      = "~/bin/jcode-tool-policy"     # gate
post_tool     = ""                            # observer
post_tool_feedback = "~/bin/jcode-vale"       # feeds output back to the model
user_prompt_submit = "~/bin/jcode-prompt-context"  # adds context to a prompt
pre_tool_timeout_ms = 5000
post_tool_feedback_timeout_ms = 10000
user_prompt_submit_timeout_ms = 30000
```

All nine command keys (`pre_tool_transform`, `user_prompt_submit`,
`turn_start`, `turn_end`, `session_start`, `session_end`, `pre_tool`,
`post_tool`, and `post_tool_feedback`) accept either a command string or an
array of strings. Unknown keys under `[hooks]` are ignored, so a config that
names a hook from a newer jcode still loads on an older one.
Independent plugins can share an event without a dispatcher:

```toml
[hooks]
turn_end = ["~/bin/jcode-turn-notify", "~/bin/jcode-event-log"]
pre_tool = ["~/bin/jcode-tool-policy", "~/bin/jcode-audit-policy"]
```

Empty or whitespace-only commands are skipped. Observer commands are spawned
in declaration order but run independently, so completion order is unspecified.
Gate commands run sequentially in declaration order, even after a block, and
the first blocking result is returned. Transformers, feedback hooks, and
prompt hooks also run sequentially in declaration order, as described below.

Env overrides (always win; empty value disables a config hook):
`JCODE_HOOK_TURN_START`, `JCODE_HOOK_TURN_END`, `JCODE_HOOK_SESSION_START`, `JCODE_HOOK_SESSION_END`,
`JCODE_HOOK_PRE_TOOL`, `JCODE_HOOK_POST_TOOL`, `JCODE_HOOK_PRE_TOOL_TIMEOUT_MS`,
`JCODE_HOOK_POST_TOOL_FEEDBACK`, `JCODE_HOOK_POST_TOOL_FEEDBACK_TIMEOUT_MS`,
`JCODE_HOOK_PRE_TOOL_TRANSFORM`, `JCODE_HOOK_PRE_TOOL_TRANSFORM_TIMEOUT_MS`,
`JCODE_HOOK_USER_PROMPT_SUBMIT`, `JCODE_HOOK_USER_PROMPT_SUBMIT_TIMEOUT_MS`.
Command overrides accept a command string or a TOML array of strings, for
example `JCODE_HOOK_TURN_END='["notify-one", "notify-two"]'`.

## Common contract

- The hook command line is parsed shell-style (quotes and backslash escapes
  work) but executed **directly**, not through a shell. A leading `~/` in the
  program path is expanded.
- The hook runs in the session working directory when known.
- Every hook receives:

| Variable | Meaning |
| --- | --- |
| `JCODE_HOOK_EVENT` | `turn_start`, `turn_end`, `session_start`, `session_end`, `pre_tool`, `post_tool`, `post_tool_feedback`, `pre_tool_transform`, `user_prompt_submit` |
| `JCODE_HOOK_SESSION_ID` | Session the event belongs to |
| `JCODE_HOOK_CWD` | Session working directory |
| `JCODE_HOOK_PAYLOAD` | JSON object mirroring all fields (capped at 16 KB) |
| `JCODE_HOOKS_DISABLED` | Always `1`; suppresses hooks in nested jcode calls (recursion guard) |

The turn and session hooks (`user_prompt_submit`, `turn_start`, `turn_end`,
`session_start`, `session_end`) also receive:

| Variable | Meaning |
| --- | --- |
| `JCODE_HOOK_TRANSCRIPT_PATH` | Absolute path of the session snapshot file. See [SESSION_FORMAT.md](SESSION_FORMAT.md) for how to read it together with its journal |
| `JCODE_HOOK_PARENT_SESSION_ID` | Parent session id. Only set for a child session |

### JSON event

Every field is also available as one JSON object: `event`, `session_id`,
`cwd`, and each `JCODE_HOOK_<NAME>` variable under the lowercased key `<name>`
(for example `transcript_path`). All values are strings.

- `JCODE_HOOK_PAYLOAD` holds that object, cut off at 16 KB. A cut-off value is
  not valid JSON.
- Observer hooks and `user_prompt_submit` receive the complete object on
  **stdin**, with no size cap. Prefer stdin when the hook parses JSON.
- `pre_tool`, `post_tool_feedback`, and `pre_tool_transform` use stdin for the
  tool input instead.

## Observer hooks

`turn_start`, `turn_end`, `session_start`, `session_end`, and `post_tool` are
**observers**: spawned detached, fire-and-forget. The agent does not wait for
their completion; failures are only logged. Stdout and stderr are discarded,
so an observer cannot return anything to the model or the user. Each observer
gets the [JSON event](#json-event) on stdin.

### `turn_start`

Fires before model generation and the first tool call in the `chat` entrypoint
(which uses the streaming turn loop) and one-shot `run` path. Extra fields:
`JCODE_HOOK_MODEL` and `JCODE_HOOK_SOURCE` (`chat` or `run`, respectively).

`JCODE_HOOK_SOURCE` is `failover` when the server re-runs the current turn on
another route after the previous account hit a usage limit. No new user
message exists for that run, and `turn_end` fires again when it finishes.

There are no distinct `resume` or `ambient` source values dispatched by the
current implementation. Those labels in older config comments are not a
runtime guarantee.

### `turn_end`

Fires when an agent turn completes (streaming turn path, which covers TUI,
desktop, swarm workers, and headless sessions, plus one-shot `jcode run`).

Extra fields: `JCODE_HOOK_STATUS` (`ok`/`error`), `JCODE_HOOK_DURATION_MS`,
`JCODE_HOOK_MODEL`, `JCODE_HOOK_LAST_ASSISTANT_TEXT` (first 4000 chars),
`JCODE_HOOK_PROMPT_TOKENS`, `JCODE_HOOK_INPUT_TOKENS`,
`JCODE_HOOK_OUTPUT_TOKENS`, `JCODE_HOOK_CACHE_READ_TOKENS`,
`JCODE_HOOK_CACHE_WRITE_TOKENS`, and
`JCODE_HOOK_ERROR` (on failure).

Token fields are summed from assistant messages in the current turn. `PROMPT_TOKENS` is the provider-reported full prompt size when available; `INPUT_TOKENS` is the raw provider-reported input count.
These values are not normalized across providers; for example, cached input may be included in the OpenAI input count
but is reported separately by Anthropic. Cache fields are omitted when no
assistant message in the turn reports that metric.

### `session_start` / `session_end`

`session_start` fires when an agent session becomes active, with
`JCODE_HOOK_SOURCE` = `create` (brand new), `attach` (existing session object
attached), or `resume` (restored by id). `session_end` fires on normal close
(`JCODE_HOOK_SOURCE=close`).

### `post_tool`

Fires after every tool call. Extra fields:

| Variable | Meaning |
| --- | --- |
| `JCODE_HOOK_TOOL_NAME` | Resolved tool name |
| `JCODE_HOOK_TOOL_CALL_ID` | Provider tool call id |
| `JCODE_HOOK_TOOL_INPUT` | Tool input JSON (capped at 16 KB) |
| `JCODE_HOOK_TOUCHED_PATHS` | Newline-separated absolute paths named by the input (`file_path`, `path`, `paths`, `apply_patch` headers, ...) |
| `JCODE_HOOK_STATUS` | `ok` / `error` |
| `JCODE_HOOK_DURATION_MS` | Tool execution time |
| `JCODE_HOOK_OUTPUT_BYTES` | Output size (on success) |
| `JCODE_HOOK_ERROR` | Error message (on failure) |

`JCODE_HOOK_TOUCHED_PATHS` is derived from the tool input, so it lists paths a
tool was asked to read or write. It does not track files a `bash` command
happened to modify.

## Feedback hook: `post_tool_feedback`

`post_tool_feedback` runs **synchronously after every successful tool call**
and can add text to the result the model sees. It gets the same env fields as
`post_tool` plus the full tool input JSON on stdin.

- Trimmed **stdout** is appended to the tool result under a
  `[post_tool_feedback]` marker (capped at 8 KB per hook). Empty stdout adds
  nothing.
- The exit code is ignored, since linters usually exit non-zero when they
  report findings. Stderr is discarded.
- Timeouts (`post_tool_feedback_timeout_ms`, default 10s) and spawn failures
  add nothing and are logged.
- Multiple hooks run in declaration order and their outputs are concatenated.

Keep it fast and filter early: it runs on every tool call, including reads.

```bash
#!/usr/bin/env bash
# ~/bin/jcode-vale: lint prose the agent just wrote
case "$JCODE_HOOK_TOOL_NAME" in
  write|edit|multiedit|apply_patch) ;;
  *) exit 0 ;;
esac
while IFS= read -r path; do
  case "$path" in
    *.md|*.mdx|*.txt|*.rst) [ -f "$path" ] && vale --output=line "$path" ;;
  esac
done <<<"$JCODE_HOOK_TOUCHED_PATHS"
exit 0
```

## Prompt hook: `user_prompt_submit`

`user_prompt_submit` runs **synchronously once per user prompt** and can add
context for the model. It fires after the user message is saved and before
`turn_start`, in both the `chat` and one-shot `run` paths.

It fires only for a prompt the user submitted. It does not fire for:

- synthetic continuations, such as the reminder-only turn after a reload
- machine-authored messages (background-task wakes, swarm and lane updates,
  notifications), which are stored with a `display_role`
- failover retries, which re-run the turn without a new message
- an empty prompt

**Input.** One JSON object on stdin, uncapped, with string values:

```json
{
  "event": "user_prompt_submit",
  "session_id": "session_fox_1794927464955_439e5d418915c243",
  "cwd": "/home/me/project",
  "prompt": "where do we deploy?",
  "model": "claude-fable-5-2",
  "source": "chat",
  "transcript_path": "/home/me/.jcode/sessions/session_fox_1794927464955_439e5d418915c243.json",
  "parent_session_id": "session_owl_1794920000000_0123456789abcdef"
}
```

`prompt` is the text the user typed, without any suffix jcode adds for the
model. It is delivered on stdin only: there is no `JCODE_HOOK_PROMPT` variable
and `JCODE_HOOK_PAYLOAD` omits it. `source` is `chat` or `run`.
`parent_session_id` is present only for a child session. The other fields are
also set as `JCODE_HOOK_*` variables.

**Output.** On exit 0 the hook may print one JSON object on stdout:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "UserPromptSubmit",
    "additionalContext": "Deploy target is fly.io (decided 2026-11-02)."
  },
  "systemMessage": "Recalled 1 memory"
}
```

- `hookSpecificOutput.additionalContext` (string, capped at 32 KB per hook) is
  appended to the transcript right after the prompt, wrapped in
  `<system-reminder>` tags. The model sees it on this turn and on every later
  turn. It is stored with `display_role: "system"`, so clients render it as a
  system entry and transcript readers can tell it apart from user text.
- `systemMessage` (string) is meant for the user and is never sent to the
  model. jcode currently writes it to the log only. It is not shown in the UI.
- Other fields, including `hookEventName`, are ignored.

The context is a message at the tail of the transcript, not a change to the
prompt or the system prompt, so it does not invalidate the cached conversation
prefix.

**Failures add nothing and never block the prompt:** a timeout
(`user_prompt_submit_timeout_ms`, default 30s), a non-zero exit, a spawn
failure, stdout that is not a JSON object, or a missing `additionalContext`.
Stderr is discarded. Multiple hooks run in declaration order and their
contexts are joined by a blank line into one message.

The model does not start until every hook has exited, so the user waits for
the slowest one on each prompt. Keep the common case fast.

```bash
#!/usr/bin/env bash
# ~/bin/jcode-prompt-context: add the current branch to every prompt
event=$(cat)
cwd=$(jq -r '.cwd // empty' <<<"$event")
branch=$(git -C "${cwd:-.}" branch --show-current 2>/dev/null) || exit 0
[ -n "$branch" ] || exit 0
jq -n --arg ctx "Current git branch: $branch" \
  '{hookSpecificOutput: {additionalContext: $ctx}}'
```

## Gate hook: `pre_tool`

`pre_tool` runs **synchronously before every tool call** and can block it:

- The hook receives `JCODE_HOOK_TOOL_NAME` plus the full tool input JSON on
  **stdin** (and a 16 KB-truncated copy in `JCODE_HOOK_TOOL_INPUT`).
- **Exit 0**: allow the call.
- **Exit 2**: block the call. The hook's stderr (trimmed, capped at 2000
  chars) is returned to the model as the tool error, so the model can adapt.
- **Anything else fails open** with a logged warning: other exit codes,
  timeout (`pre_tool_timeout_ms`, default 5s), missing binary, spawn errors.

### JSON output (Codex/Claude compatible)

On exit 0 a gate may print one JSON object on stdout. Non-JSON stdout is
ignored, so plain exit-code scripts keep working. Recognized fields:

```json
{
  "hookSpecificOutput": {
    "permissionDecision": "deny",
    "permissionDecisionReason": "rm -rf is not allowed",
    "updatedInput": { "command": "ls" },
    "additionalContext": "note shown to the model"
  },
  "systemMessage": "also shown to the model"
}
```

- `permissionDecision: "deny"` (or legacy top-level `"decision": "block"`
  with `reason`) blocks the call with that reason.
- `permissionDecision: "ask"` also blocks, with a "requires user approval"
  reason. jcode has no interactive approval prompt for hooks yet.
- `updatedInput` (an object) replaces the tool input. With several gates,
  later gates receive the rewritten input. A block discards rewrites.
- `additionalContext` and `systemMessage` are appended to the tool result
  (or block error) under a `[pre_tool context]` marker.

Fail-open is deliberate: a broken policy script should degrade to "no policy"
rather than brick every session. If you need fail-closed semantics, make the
hook itself robust (it is your trust boundary, not jcode).

## Tool-input transformers: `pre_tool_transform`

Transformers are synchronous external plugins that can replace a tool's JSON
input before it is validated and executed. Configure one or more in order:

```toml
[hooks]
pre_tool_transform = ["~/.jcode/plugins/rtk-transform"]
pre_tool_transform_timeout_ms = 500
```

Each transformer receives the current complete tool-input JSON on stdin and
`JCODE_HOOK_TOOL_NAME`, `JCODE_HOOK_SESSION_ID`, and `JCODE_HOOK_CWD` in its
environment. It may print a replacement JSON object to stdout. Empty stdout,
invalid JSON, a non-zero exit, a spawn error, or a timeout leaves the input
unchanged. This fail-open contract makes optional integrations safe to enable.

Transformers compose in configuration order and run before the `pre_tool`
policy gate, so policies evaluate the final transformed input. Keep plugins
small and tool-specific. For example, an RTK plugin should only change
`bash.command` when `rtk rewrite` returns a replacement.

### Example policy script

```bash
#!/usr/bin/env bash
# ~/bin/jcode-tool-policy
# stdin: tool input JSON. Env: JCODE_HOOK_TOOL_NAME, JCODE_HOOK_SESSION_ID...
input=$(cat)

case "$JCODE_HOOK_TOOL_NAME" in
  bash)
    if grep -qE 'rm -rf /([^a-zA-Z]|$)|mkfs|dd if=' <<<"$input"; then
      echo "blocked: destructive shell command" >&2
      exit 2
    fi
    ;;
  write|edit)
    if grep -q '"file_path":"/etc/' <<<"$input"; then
      echo "blocked: writes to /etc are not allowed" >&2
      exit 2
    fi
    ;;
esac
exit 0
```

## Example: tmux status + desktop notification on turn end

```bash
#!/usr/bin/env bash
# ~/bin/jcode-turn-notify
if [ "$JCODE_HOOK_STATUS" = ok ]; then icon=✅; else icon=❌; fi
tmux display-message "jcode $icon ${JCODE_HOOK_SESSION_ID:0:12}" 2>/dev/null
notify-send "jcode turn $JCODE_HOOK_STATUS" \
  "${JCODE_HOOK_LAST_ASSISTANT_TEXT:0:120}" 2>/dev/null
exit 0
```

## Example: JSON event log of all hook activity

Point several hooks at one script and fan out on `JCODE_HOOK_EVENT`:

```bash
#!/usr/bin/env bash
# ~/bin/jcode-event-log
# Observers get the complete event as one JSON object on stdin.
{ cat; echo; } >> ~/.local/state/jcode-events.jsonl
```

```toml
[hooks]
turn_end      = "~/bin/jcode-event-log"
session_start = "~/bin/jcode-event-log"
session_end   = "~/bin/jcode-event-log"
post_tool     = "~/bin/jcode-event-log"
```

## Design notes

- Hook lookups are config-driven and re-read on config reload; you can add or
  change hooks without restarting jcode.
- Hot paths (`pre_tool`/`post_tool`) check whether a hook is configured before
  building any payload, so unconfigured hooks cost ~nothing.
- The recursion guard (`JCODE_HOOKS_DISABLED=1`) means a hook may safely call
  `jcode` CLI commands without re-triggering hooks in that nested process.
