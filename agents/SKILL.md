---
name: token-tracker
description: Saves local token usage snapshots and reports token usage by feature with a daily heat map. Use when the user invokes /token-tracker or $token-tracker; asks to save, record, dump, log, or track token usage/history; wants a usage breakdown or heat map; discusses token usage or cost; or wants to label the current project or feature.
---

# Token Tracker

Works across Cursor, Claude Code, Gemini CLI, Codex, Continue, and other Agent Skills hosts. Shared history lives under `~/.cursor/token-tracker/`.

## Slash / skill invoke: token-tracker

When the user invokes `/token-tracker` or `$token-tracker` (with no other request), run the report and show the output:

```bash
~/.agents/skills/token-tracker/scripts/report-token-usage.js
```

Present the report as-is (feature breakdown + GitHub-style daily heat map). Then ask once:

`Also save a usage snapshot for the current feature?`

Default to not saving if they do not answer.

If the invoke includes an explicit ask (e.g. set feature, save, enable status line), do that instead of or in addition to the report.

## When To Prompt (manual save)

Prompt the user before saving unless they explicitly asked to save.

Ask:

`Save this token usage snapshot to local history?`

Default to not saving if the user does not answer. Do not save secrets, raw prompts, or transcript contents.

## Save Workflow

1. Build a short snapshot:
   - `project`: workspace directory name or current project name.
   - `feature`: configured feature name, current git branch, or omit if unknown.
   - `summary`: one sentence describing the session or task.
   - `model`: current model name if known.
   - `prompt_tokens`, `completion_tokens`, `total_tokens`: include exact values only when available.
   - `metadata`: optional small object for non-sensitive details.
2. Run:

```bash
~/.agents/skills/token-tracker/scripts/save-token-usage.js --json '<snapshot-json>'
```

3. Tell the user the snapshot was saved to `~/.cursor/token-tracker/history.jsonl`.

## Snapshot Rules

- Save summaries, not conversation content.
- If token counts are unavailable, save the summary with `source: "manual"` and omit the unknown fields.
- Status line tokens are feature-scoped: switching project/feature resets the token counter for that scope.
- Status line snapshots use `source: "statusline"` and store the current feature token total (not full session total).
- If the user asks for project/feature history, run the report script or summarize `~/.cursor/token-tracker/history.jsonl`. For a feature total, prefer the report (epoch-aware) over naively summing rows.

## Project And Feature Names

Project names resolve in this order:

1. `TOKEN_TRACKER_PROJECT` environment variable.
2. Exact workspace path in `~/.cursor/token-tracker/config.json` under `projects`.
3. `default_project` in `~/.cursor/token-tracker/config.json`.
4. Current workspace folder name.

Feature names resolve in this order:

1. `TOKEN_TRACKER_FEATURE` environment variable.
2. Exact workspace path in `~/.cursor/token-tracker/config.json` under `features`.
3. `default_feature` in `~/.cursor/token-tracker/config.json`.
4. Current git branch (`git branch --show-current`), or the host's worktree/branch name when available.

Set feature:

```bash
~/.agents/skills/token-tracker/scripts/set-token-context.js --workspace "$PWD" --feature "maintenance"
```

## Status Line (Cursor CLI)

```bash
~/.agents/skills/token-tracker/scripts/statusline-token-usage.js
```

Fields are controlled by `~/.cursor/token-tracker/config.json` under `statusline`. Cursor CLI can wire this via `cli-config.json` `statusLine`. Other hosts can still call the same script when they expose a status hook.
