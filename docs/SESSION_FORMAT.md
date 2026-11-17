# Session File Format

jcode stores each session's transcript on disk as JSON. This page describes
those files as a **read interface** for external tools: memory systems,
exporters, analytics, and [lifecycle hooks](HOOKS.md).

The interface is read-only. Do not write, rename, or delete these files while
jcode may be running. To add context to a session, use the
[`user_prompt_submit` hook](HOOKS.md#prompt-hook-user_prompt_submit).

## Files

Sessions live in `$JCODE_HOME/sessions/`, or `~/.jcode/sessions/` when
`JCODE_HOME` is not set.

| File | Content |
| --- | --- |
| `<session_id>.json` | Snapshot. One JSON object with session metadata and a `messages` array. |
| `<session_id>.journal.jsonl` | Journal. Messages added since the snapshot was written, one JSON object per line. Often absent. |
| `<session_id>.bak` | Previous snapshot. Not a transcript source; ignore it. |

Ignore every other file in the directory.

`<session_id>` has the form `session_<name>_<epoch_ms>_<hex>`. Hooks receive
the snapshot path as `transcript_path` (`JCODE_HOOK_TRANSCRIPT_PATH`). The
journal path is the snapshot path with `.json` replaced by `.journal.jsonl`.

## Reading a transcript

**The snapshot alone is not the transcript.** jcode appends new messages to
the journal and rewrites the snapshot only at checkpoints, so an active
session usually has most of its recent messages in the journal.

1. Parse the snapshot and take its `messages` array. If the file is missing or
   does not parse, start from an empty list.
2. If the journal exists, read it line by line. Skip a line that does not
   parse and continue with the next one.
3. For each journal line, append every entry of `append_messages` whose `id`
   has not been seen yet.

Step 3 must deduplicate by `id`. A checkpoint writes the snapshot and then
deletes the journal, and a crash between those two steps leaves messages in
both files. Message ids are unique within a session.

The result is the session's messages in order. The files can change while you
read them. A reader that runs from a `turn_end` or `session_end` hook sees the
finished turn, because jcode saves the final assistant message before it fires
the hook.

## Snapshot

Top-level fields a reader can rely on:

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | Session id. Matches the file name. |
| `parent_id` | string or null | Session this one was spawned or forked from. |
| `title` | string or null | Generated title. |
| `created_at`, `updated_at` | RFC 3339 string | Timestamps in UTC. |
| `working_dir` | string, optional | Session working directory. |
| `status` | string or object | `"Active"`, `"Closed"`, `"Reloaded"`, `"Compacted"`, `"RateLimited"`, or an object keyed by `Crashed` or `Error`. |
| `model` | string, optional | Model id. |
| `is_debug` | boolean | True for test and debug sessions. |
| `messages` | array | Messages, oldest first. See below. |

The snapshot has more fields. Treat any field not listed here as internal.

## Journal

Each line is one object:

```json
{"meta": {"updated_at": "2026-11-17T19:41:28.341465Z", "status": "Active", "working_dir": "/home/me/project"},
 "append_messages": [{"id": "message_1794927531331_9536177627331384962", "role": "assistant", "content": []}]}
```

- `append_messages` (optional array) holds messages in the same shape as the
  snapshot's `messages`. A line without it changes metadata only.
- `meta` repeats session metadata as of that line. The last parsed line has
  the newest `title`, `status`, and `working_dir`.
- Ignore other keys.

## Messages

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | Unique within the session. |
| `role` | `"user"` or `"assistant"` | Role sent to the model. |
| `content` | array | Content blocks. See below. |
| `display_role` | string, optional | Set on machine-authored messages. See below. |
| `timestamp` | RFC 3339 string, optional | When the message was added. |
| `tool_duration_ms` | number, optional | Tool run time, on tool result messages. |
| `token_usage` | object, optional | Provider token counts, on assistant messages. |

### `display_role`

A `user` message without `display_role` is text the user typed. A message with
`display_role` was written by jcode, not by the user, even though its `role`
is `user`:

| Value | Used for |
| --- | --- |
| `system` | Session context, `<system-reminder>` messages (including `user_prompt_submit` hook context), notifications from other agents, `[auto]` continuations, tool and skill announcements. |
| `background_task` | Background task results that wake the agent. |
| `file_activity` | Notices about files another agent changed. |

A reader that wants the human conversation skips every message that has a
`display_role`. This also keeps a memory tool from storing context it injected
itself.

Under swarm effort, jcode appends the line
`Use swarms to get things done in parallel.` to the user's own message. Strip
it if you need the exact text the user typed.

### Content blocks

Each block is an object tagged by `type`:

| `type` | Fields | Meaning |
| --- | --- | --- |
| `text` | `text` | Prose from the user or the assistant. |
| `tool_use` | `id`, `name`, `input` | Assistant tool call. `input` is the tool's JSON arguments. |
| `tool_result` | `tool_use_id`, `content`, `is_error` (optional) | Tool output, in a `user` message. |
| `image` | `media_type`, `data` | Base64 image attached to a message. |
| `reasoning`, `reasoning_trace`, `anthropic_thinking`, `openai_reasoning` | varies | Model reasoning. May be encrypted. |
| `openai_compaction`, `provider_native`, `tool_reference` | varies | Provider-specific state. |

A transcript reader normally needs `text`, `tool_use`, and `tool_result` only.
Skip the reasoning and provider-specific blocks: their fields are not part of
this interface.

## Example

A user prompt, the context a `user_prompt_submit` hook added, and the
assistant's reply with a tool call:

```json
[
  {"id": "message_1794927525589_7086461200253818326", "role": "user",
   "timestamp": "2026-11-17T14:58:45.589418Z",
   "content": [{"type": "text", "text": "where do we deploy?"}]},
  {"id": "message_1794927525601_1163850931204557190", "role": "user", "display_role": "system",
   "timestamp": "2026-11-17T14:58:45.601027Z",
   "content": [{"type": "text", "text": "<system-reminder>\nDeploy target is fly.io.\n</system-reminder>"}]},
  {"id": "message_1794927531331_9536177627331384962", "role": "assistant",
   "timestamp": "2026-11-17T14:58:51.331048Z",
   "content": [
     {"type": "text", "text": "Checking the deploy config."},
     {"type": "tool_use", "id": "toolu_01Cqv7fMMeUYLkdb1rcERayf", "name": "read",
      "input": {"file_path": "fly.toml"}}]}
]
```

## Compatibility

- The format has no version field. Changes are additive: jcode adds fields,
  block types, and `display_role` values, and does not remove or repurpose the
  ones documented here.
- Readers must ignore fields, block types, `status` values, and `display_role`
  values they do not know. Treat an unknown `display_role` as machine-authored.
- Everything not described on this page is internal and can change in any
  release.
