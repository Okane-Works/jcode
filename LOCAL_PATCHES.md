# Local patches

This checkout carries local commits on top of `upstream/master` (1jehuang/jcode).
`workday upgrade` rebases them onto upstream each morning. Read this file before
resolving a rebase conflict: it records the intent of each patch, which the diff
alone does not.

## Remotes and branch model

- `upstream`: https://github.com/1jehuang/jcode, read only.
- `origin`: the fork, https://github.com/Okane-Works/jcode. Its `master` is
  org-protected (PRs only, no force-push), so the patch stack lives on
  `origin/okane`. Local `master` tracks it and is pushed with
  `--force-with-lease` after every rebase.
- `master` is always `upstream/master` plus the patches below, in this order.
- `git rerere` is enabled. A conflict resolved once is replayed automatically.

## Auto-update

Source builds only auto-update by fast-forwarding `master` from its tracking
branch, and skip it entirely when local commits are ahead of upstream
(`src/cli/startup.rs`, `hot_exec::local_commits_ahead_of_upstream`). With
`master` tracking `origin/okane`, jcode never updates itself; workday owns updates.
The tooling is `~/.local/bin/jcode-sync` (prepare, swap, rollback, status),
called by `workday upgrade`. Conflict reports go to `~/.jcode/sync/conflict-report.md`.

## Resolving a conflict

1. Find the patch below whose files conflict and read its intent.
2. Keep upstream's change and re-apply the intent on top of it. Don't restore
   the old code wholesale.
3. If upstream now does the same thing, drop the patch and delete its entry.
4. Build, run the patch's tests, then `git rebase --continue`.
5. Update the entry if files or behavior changed.

## Patches

### 1. Server build fix
- Commit: `fix(server): drop call to nonexistent agents_status::todos_updated`
- Files: `crates/jcode-app-core/src/server.rs`
- Intent: removes a call to a function that does not exist upstream, which broke
  the build.
- Retire when: upstream builds without it. Likely already fixed upstream; drop
  on the first rebase if it conflicts or becomes empty.

### 2. OpenRig harness integration
- Commits: OpenRig launch protocol, activity reporting and `--effort`/`--fast`
  flags, base preset docs, per-seat model and identity fix.
- Files: `crates/jcode-tui/src/openrig.rs`, `crates/jcode-tui/src/openrig_activity.rs`
  (new), hooks in `crates/jcode-tui/src/tui/app*.rs`, `tui/backend.rs`,
  `src/cli/{args,dispatch,tui_launch}.rs`, `docs/openrig/`, and the Anthropic
  runtime (`crates/jcode-provider-anthropic-runtime/src/{lib,reasoning_request,lib_stream_retries,usage_recording}.rs`).
- Intent: jcode runs as an OpenRig community-harness seat. It speaks the launch
  protocol, reports activity (busy, idle, waiting) to OpenRig, keeps idle panes
  quiet, accepts per-seat model, identity, `--effort` and `--fast` flags, and
  sends the current Anthropic fast-mode request shape.
- Conflict hot spots: TUI app lifecycle and remote event handling, CLI
  dispatch, and Anthropic request building. Keep the hook calls small and
  re-insert them at upstream's equivalent points.
- Tests: `cargo test -p jcode-tui openrig`, `cargo test -p jcode-provider-anthropic-runtime`.

### 3. MCP config precedence
- Commit: `fix(mcp): let ~/.jcode/mcp.json win over Claude Code global config`
- Files: `crates/jcode-base/src/mcp/protocol.rs`, `protocol_tests.rs`
- Intent: when a server is defined in both `~/.jcode/mcp.json` and Claude
  Code's global config, jcode's own file wins.
- Tests: `cargo test -p jcode-base mcp`.

### 4. `user_prompt_submit` hook and stdin events
- Commit: `feat(hooks): add user_prompt_submit and send observers the event on stdin`
- Files: `crates/jcode-base/src/hooks.rs`, `config*.rs`,
  `crates/jcode-config-types/src/lib.rs`, `crates/jcode-app-core/src/agent*.rs`
- Intent: a hook that runs when the user submits a prompt, and observer hooks
  receive the event as JSON on stdin. Used by local memory and observer tooling.
- Tests: `cargo test -p jcode-base hooks`, `cargo test -p jcode-app-core system_prompt_reminder`.

### 5. Docs
- Commits: hooks and session-format docs.
- Files: `docs/HOOKS.md`, `docs/SESSION_FORMAT.md`
- Intent: documents patch 4 and the session file format. Doc conflicts: keep
  upstream text and re-add our sections.

### 6. Headroom proxy routing for seat (OAuth) traffic
- Commits: `feat(openai): allow JCODE_CHATGPT_API_BASE ...`,
  `feat(anthropic): allow JCODE_CLAUDE_OAUTH_API_BASE ...`,
  `feat(config): provider.chatgpt_api_base / claude_oauth_api_base`.
- Files: `crates/jcode-base/src/provider/openai.rs`, `crates/jcode-base/src/sidecar.rs`,
  `crates/jcode-provider-openai-runtime/src/lib.rs` (`responses_url`),
  `crates/jcode-provider-anthropic-runtime/src/lib.rs` (`oauth_api_url`),
  `crates/jcode-config-types/src/lib.rs` (`ProviderConfig`),
  `crates/jcode-base/src/config.rs` + `config/env_overrides.rs`.
- Intent: upstream hardcodes chatgpt.com and api.anthropic.com for Codex and
  Claude subscription seats. These opt-in settings let seat traffic go through
  the local Headroom proxy daemon. Set in `~/.jcode/config.toml` `[provider]`:
  `chatgpt_api_base = "http://127.0.0.1:8787/v1"`,
  `claude_oauth_api_base = "http://127.0.0.1:8787"`. Config, not env, so the
  menubar-launched server picks it up. Unset means upstream behavior.
- Tests: `cargo test -p jcode-base chatgpt_base`,
  `cargo test -p jcode-provider-anthropic-runtime oauth_api_url`.

### 7. Token efficiency for long-lived OpenRig seats
- Commits: `feat(bash): shape output ...` (bundled in `10f08191f`),
  `perf(todo): return only changed items ...`,
  `feat(compaction): stub large stale tool results ...`,
  `feat(compaction): stub stale large tool results in API requests`, plus the
  per-seat `JCODE_COMPACTION_*` env override commits.
- Files: `crates/jcode-app-core/src/tool/bash.rs`, `tool/bash_output_shape.rs`,
  `tool/todo.rs`, `crates/jcode-base/src/compaction.rs`,
  `crates/jcode-config-types/src/lib.rs` (`CompactionConfig`),
  `crates/jcode-base/src/config/env_overrides.rs`.
- Intent: OpenRig seats grew past 350k tokens. Bash output is cleaned (ANSI,
  progress bars, repeated lines) and cut to head+tail at 12k chars, with the full output saved to
  scratch (`JCODE_BASH_OUTPUT_MAX_CHARS`, 0 = legacy). Todo patch writes return
  only deltas. Old large tool results are stubbed in outgoing requests only
  (`[compaction] stale_tool_result_turns/max_chars`, 0 disables). Per-seat env
  overrides let implementer seats compact earlier than leads.
- Tests: `cargo test -p jcode-app-core bash_`, `cargo test -p jcode-app-core todo`,
  `cargo test -p jcode-base compaction`.
- Related local config (not code): `~/.jcode/hooks/rtk-rewrite.py` wired as
  `[hooks] pre_tool_transform` routes bash through `rtk rewrite`.
