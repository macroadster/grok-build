# Enhancement: Worker Subagent Productivity

| | |
|---|---|
| **Status** | Proposed |
| **Product** | Grok Build |
| **Date** | 2026-07-15 |
| **Source session** | Go quiz orchestration in `quiz-go` (manager + worker/watcher roles) |
| **Audience** | Implementers fixing subagent tooling and manager/worker orchestration |

---

## Table of contents

1. [Summary](#1-summary)
2. [Problem statement](#2-problem-statement)
3. [Goals and non-goals](#3-goals-and-non-goals)
4. [Proposed enhancements](#4-proposed-enhancements)
5. [Architecture sketch](#5-architecture-sketch)
6. [Target orchestration pattern](#6-target-orchestration-pattern)
7. [Testing plan](#7-testing-plan)
8. [Documentation updates](#8-documentation-updates)
9. [Priority and sequencing](#9-priority-and-sequencing)
10. [Success metrics](#10-success-metrics)
11. [Appendix A — Incident detail](#11-appendix-a--incident-detail)
12. [Appendix B — PR acceptance checklist](#12-appendix-b--pr-acceptance-checklist)
13. [Open questions](#13-open-questions)

---

## 1. Summary

Workers are documented and prompted as the primary **implementation** agents: edit files, run builds and tests, report results. In practice they are often unusable for write or execute work because agent construction can fail on an inconsistent tool graph. Managers then fall back to `general-purpose`, which defeats the role model.

This enhancement makes three things true:

1. **Workers spawn reliably** — any advertised capability that includes shell builds a valid tool graph (or safely degrades).
2. **Capabilities are honest** — role defaults and capability modes match the tools the agent actually receives.
3. **Orchestration is artifact-first** — workers write results to disk and return structured summaries; managers coordinate and merge instead of ferrying large prompts.

**Primary outcome:** A manager can spawn `worker` for implement/write tasks without spawn failures, and workers return structured results plus on-disk artifacts that are easy to merge and verify.

---

## 2. Problem statement

### 2.1 Blocking failure

Spawning a **worker** to write files failed at **agent build time** (0 tool calls, ~0s duration):

```text
agent building failed: tool error: Requirements unsatisfied:
  tool: GrokBuild:run_terminal_cmd
  message: enabled_background=true requires GrokBuild:get_task_output
           and GrokBuild:kill_task so background bash tasks can be
           observed and cancelled
  field_path: params.enabled_background
  expected: set enabled_background=false OR include get_task_output and kill_task
```

**Interpretation**

| Fact | Detail |
|------|--------|
| Trigger | Worker toolsets enable `run_terminal_cmd` with `enabled_background=true` |
| Dependency | Background bash requires companion tools on the **same** agent: `get_task_output` and `kill_task` (or equivalents `get_command_or_subagent_output` / `kill_command_or_subagent`) |
| Gap | Companions were missing from the worker toolset (or stripped by capability mode / role resolution) |
| Timing | Validation runs at **session build**, so the worker never starts and the manager cannot recover inside the child |

### 2.2 Asymmetry that hides the bug

| Spawn | Type | Capability | Result |
|-------|------|------------|--------|
| Answer problems 001–100 (5× parallel) | `worker` | `read-only` | Success |
| Write `answers.md` | `worker` | default / write | **Spawn failure** (tool requirements) |
| Write `answers.md` | `general-purpose` | `read-write` | Success |
| Grade → `results.md` | `general-purpose` | `read-write` | Success |
| Verify scoring | `watcher` | `read-only` | Success |

Read-only workers never hit the broken bash graph. Write and implement workers do. Managers internalize an accidental rule: **do not use worker for real work; use general-purpose.** That defeats the role model.

### 2.3 Orchestration tax (productivity, not only reliability)

Even when workers succeed, manager productivity suffers:

| Tax | Symptom |
|-----|---------|
| **Prompt ferrying** | Parent pastes large tables or file bodies into the next spawn instead of merging on-disk artifacts |
| **Unstructured returns** | Free-form markdown requires re-parsing; easy to drop rows or mistranscribe |
| **No partial progress** | Kill or failure on a long worker loses intermediate work unless files were written early |
| **Opaque spawn errors** | Requirements failures are engineer-facing, not role-facing |
| **Role / tool mismatch** | Docs and system prompts say workers implement; runtime tool graphs sometimes cannot |

### 2.4 Why this matters

- Managers are instructed to **delegate implementation to workers** and **verification to watchers**.
- If workers cannot spawn for write or execute work, the manager workflow is broken or forced into non-idiomatic GP fallbacks.
- Parallel fan-out (the productive part of the session) only worked for **read-only** slices.

---

## 3. Goals and non-goals

### Goals

| # | Goal | Intent |
|---|------|--------|
| 1 | **Spawn reliability** | Any advertised `worker` capability that includes shell builds a valid tool graph, or background shell is disabled automatically |
| 2 | **Honest capabilities** | Capability modes and role defaults match actual tools — no “implementer” role that cannot edit or run tests |
| 3 | **Artifact-first handoff** | Workers write results to agreed paths; parents merge and verify rather than re-prompt full content |
| 4 | **Structured completion payload** | Machine-readable summary (files changed, commands, exit codes, errors) on every worker completion |
| 5 | **Actionable failures** | Spawn vs task vs timeout failures are distinct and human-readable for the parent model |
| 6 | **Specialist-friendly roles** | Optional narrow roles (e.g. file-writer without bash) avoid unnecessary tool-graph complexity |

### Non-goals

- Changing the one-level subagent depth limit.
- Redesigning the full manager / watcher philosophy.
- Replacing `general-purpose` (it remains the escape hatch and default full agent).

---

## 4. Proposed enhancements

### P0 — Fix worker tool-graph construction *(blocking)*

**Problem:** `enabled_background=true` on `run_terminal_cmd` without companion observe/cancel tools fails agent build.

**Requirements**

1. When building any agent or subagent toolset, enforce a **tool dependency graph**:
   - If `run_terminal_cmd.params.enabled_background == true`, then `get_task_output` (or `get_command_or_subagent_output`) **and** `kill_task` (or `kill_command_or_subagent`) **must** be present.
2. Resolution strategy (pick one primary and document it):

   | Strategy | Behavior | When to use |
   |----------|----------|-------------|
   | **Preferred** | Auto-include companion tools whenever background bash is enabled | Normal role construction |
   | **Fallback** | If companions cannot be included (policy / capability strip), set `enabled_background=false` and still spawn | Capability-constrained modes |
   | **Never** | Fail the entire agent build when a safe degrade exists | — |

3. Validate tool graphs at **agent definition / role load** (and in `/config-agents` diagnostics), not only at first spawn.
4. Unit and integration tests:

   | Case | Expected |
   |------|----------|
   | Worker + default tools | Builds |
   | Worker + `read-only` | Builds (no write; no shell per mode) |
   | Worker + `read-write` | Builds (file tools; shell absent or non-background) |
   | Worker + `execute` / `all` | Builds with background bash **and** companions |
   | Companions stripped while `enabled_background=true` | Auto-fix **or** explicit load-time diagnostics error — never silent mid-session surprise |

**Acceptance criteria**

- [ ] Spawning `subagent_type: worker` with no special flags succeeds.
- [ ] Spawning `worker` with `capability_mode: read-write` can create and edit files.
- [ ] Spawning `worker` with `capability_mode: all` (or role default implement) can run foreground and background shell, poll output, and kill tasks.
- [ ] No spawn fails solely due to `enabled_background` / missing companion mismatch.
- [ ] Automated test covers the regression from the quiz session.

---

### P0 — Distinct, readable spawn failure taxonomy

**Problem:** Parent only sees a raw `Requirements unsatisfied` blob.

**Requirements**

Classify failures returned to the parent:

| Class | Meaning | Example message |
|-------|---------|-----------------|
| `spawn_tool_graph` | Agent could not be built | “Worker spawn failed: background shell enabled but task observe/cancel tools missing. Fixed by including X/Y or disabling background.” |
| `spawn_depth` | Nested subagent forbidden | Existing depth-limit behavior |
| `spawn_config` | Unknown type, disabled type, or bad persona | “Unknown subagent_type `worker`” |
| `task_error` | Agent ran; task failed | Non-zero work or exception in child |
| `timeout` | Agent exceeded limit | Existing timeout paths |
| `cancelled` | Parent or user killed child | Existing kill paths |

**Acceptance criteria**

- [ ] Parent completion/error payload includes `failure_class` + short `failure_hint`.
- [ ] Manager-visible text is actionable without reading engine source.

---

### P1 — Honest capability modes for workers

Documented modes (user guide) already define:

| Mode | Read | Write | Execute |
|------|:----:|:-----:|:-------:|
| `read-only` | ✓ | | |
| `read-write` | ✓ | ✓ | |
| `execute` | ✓ | | ✓ |
| `all` | ✓ | ✓ | ✓ |

**Requirements**

1. **Worker default** should be implement-capable: prefer default capability `all` (or explicit role default `all`), consistent with “primary task executor.”
2. Mode filtering must **recompute** bash background flags:
   - Modes without execute → no `run_terminal_cmd` (or non-executable stub absent entirely).
   - Modes with execute → full bash lifecycle tools if background is enabled.
3. `read-write` workers must not pull in a broken half-shell config (file tools only is fine).
4. Surface the effective tool list in debug/diagnostics (`/config-agents` or spawn dry-run).

**Acceptance criteria**

- [ ] Capability matrix above matches runtime tools for `worker`.
- [ ] Manager docs and system prompts for worker match runtime defaults.
- [ ] Dry-run or diagnostics can print effective tools for `(type, capability_mode)`.

---

### P1 — Artifact-first worker contract

**Problem:** Managers re-embed large outputs into the next prompt (error-prone, token-heavy).

**Requirements**

1. Establish a **default worker output contract** in the worker system prompt (and optional persona):

   ```text
   On completion, always report:
   - summary (≤20 lines)
   - artifacts: list of paths created/updated
   - commands_run: [{cmd, exit_code}] (if any)
   - status: success | partial | failed
   - next_hints: optional bullets for parent
   ```

2. Prefer **write intermediate files early** for long tasks (e.g. `answers-partial-001-020.md`), then final paths.
3. Optional spawn parameter or convention:
   - `artifact_dir`, or document convention: `.grok/subagent-artifacts/<subagent_id>/`
   - Parent merges from known locations
4. For multi-worker fan-out, document a merge pattern:

   ```text
   N workers → partial artifacts
   1 merge worker (read-write) → final file
   1 watcher → verify
   ```

**Acceptance criteria**

- [ ] Worker system prompt includes the completion contract.
- [ ] At least one integration example (or skill snippet) shows partial artifacts + merge.
- [ ] Parent can complete a multi-worker write workflow without pasting full file bodies into spawn prompts.

---

### P1 — Structured completion payload (API / protocol)

**Problem:** Parent receives prose only; hard to automate scoring, CI, or merge.

**Requirements**

Extend subagent completion (tool result / notification) with optional structured fields:

```json
{
  "status": "success",
  "summary": "Wrote answers.md with 100 rows",
  "artifacts": ["answers.md"],
  "files_changed": ["answers.md"],
  "commands": [{"cmd": "wc -l answers.md", "exit_code": 0}],
  "metrics": {"tool_calls": 5, "duration_ms": 95800},
  "failure_class": null,
  "failure_hint": null
}
```

Workers should be instructed to emit this; the runtime may also **derive** `files_changed`, `duration_ms`, and `tool_calls` automatically.

**Acceptance criteria**

- [ ] Parent tool result includes structured fields when available.
- [ ] Runtime-derived metrics are present even if the model forgets to summarize.
- [ ] Watcher and manager can key off `artifacts` without regexing prose.

---

### P2 — Specialist roles / presets

Avoid one overloaded worker definition for every job.

| Role preset | Default capability | Shell | Use case |
|-------------|--------------------|-------|----------|
| `worker` (default) | `all` | yes + background lifecycle | Implement, test, fix |
| `worker-writer` or capability `read-write` | `read-write` | no | Pure file generation / merge |
| `worker-runner` or capability `execute` | `execute` | yes + lifecycle | Test / build only |
| `explore` / read worker | `read-only` | per product policy | Investigate, quiz-style reasoning |
| `watcher` | read + execute (no write) | yes as needed | Verify only |

**Requirements**

- Either first-class `subagent_type` values or documented `roles` in config that managers can spawn by name.
- Writer-without-bash path must work even if bash lifecycle is under repair (isolation of concerns).

**Acceptance criteria**

- [ ] Documented way to spawn a no-shell file writer that always builds.
- [ ] Default implement worker still has full power after the P0 fix.

---

### P2 — Parallelism and progress

**Requirements**

1. Soft guidance or platform limits for concurrent subagents (e.g. warn or queue above N).
2. Workers writing partial artifacts enable resume after cancel.
3. Optional: progress notifications (“completed 10/20 files”) without full transcript spam.
4. Prefer `isolation: worktree` defaults for parallel implement workers that edit the same repo.

**Acceptance criteria**

- [ ] Docs recommend worktree for parallel implementers.
- [ ] Partial artifact pattern is documented as the cancel/resume strategy.

---

### P2 — Context efficiency

**Requirements**

1. Encourage path lists + acceptance criteria in spawn prompts; workers `read_file` themselves.
2. Support `resume_from` for staged pipelines (research → implement → fix tests) without re-seeding huge context.
3. Compact worker system prompts; put long policy in loadable role files.

**Acceptance criteria**

- [ ] Manager-oriented docs show a thin spawn prompt example vs anti-pattern (paste entire file contents).

---

## 5. Architecture sketch

### 5.1 Tool graph validator

```text
fn validate_toolset(tools) -> Result<(), ToolGraphError>
  if has(run_terminal_cmd) && enabled_background(run_terminal_cmd):
    require(get_task_output_equivalent)
    require(kill_task_equivalent)

fn finalize_toolset(role, capability_mode) -> Toolset
  tools = base_tools(role)
  tools = apply_capability_filter(tools, capability_mode)
  tools = satisfy_dependencies(tools)  // add companions OR clear background flag
  validate_toolset(tools)?
  return tools
```

**Design rules**

- `satisfy_dependencies` runs **after** capability filtering so stripped companions cannot leave a half-enabled shell.
- Prefer auto-include over fail-closed when the role intends execute + background.
- Prefer disable-background over fail-closed when companions are policy-forbidden.

### 5.2 Capability filter rules *(normative)*

| Mode | File edit tools | `run_terminal_cmd` | Background flag | Task observe / kill |
|------|:---------------:|:------------------:|:---------------:|:-------------------:|
| `read-only` | no | no\* | n/a | n/a |
| `read-write` | yes | no | n/a | n/a |
| `execute` | no | yes | true (if supported) | required if background |
| `all` | yes | yes | true (if supported) | required if background |

\*If product policy allows read-only shell for `explore`, that is a separate type. Keep `worker` + `read-only` aligned with the user-guide matrix (no shell) unless deliberately documented otherwise.

### 5.3 Spawn error surface

Return to parent:

```json
{
  "ok": false,
  "failure_class": "spawn_tool_graph",
  "failure_hint": "Include get_task_output and kill_task, or disable background shell for this role.",
  "technical_detail": "Requirements unsatisfied: ..."
}
```

Keep `technical_detail` for engineers; surface `failure_class` + `failure_hint` to the parent model.

### 5.4 Artifact directory convention *(optional but useful)*

```text
.grok/subagent-artifacts/<session_id>/<subagent_id>/
  # stdout summary is still returned to parent
  # large outputs written under this dir when possible
```

Add to `.gitignore` templates if appropriate.

---

## 6. Target orchestration pattern

Example: multi-part generation → merge → verify

```text
┌─────────────────┐   ┌─────────────────┐   ┌─────────────────┐
│ worker read-only│   │ worker read-only│   │ worker read-only│
│ → partial A     │   │ → partial B     │   │ → partial C     │
└────────┬────────┘   └────────┬────────┘   └────────┬────────┘
         │                     │                     │
         └─────────────────────┼─────────────────────┘
                               ▼
                    ┌──────────────────────┐
                    │ worker read-write    │
                    │ merge → final file   │
                    └──────────┬───────────┘
                               ▼
                    ┌──────────────────────┐
                    │ watcher              │
                    │ verify acceptance    │
                    └──────────────────────┘
```

**Properties of a healthy run**

- No full-content paste through the manager.
- No `general-purpose` fallback required for simple file writes.
- Partials survive cancel; merge and verify are cheap to re-run.

---

## 7. Testing plan

### Regression *(from production incident)*

| # | Case | Expected |
|---|------|----------|
| 1 | Spawn worker, capability omitted | Builds and can write a small file |
| 2 | Spawn worker `read-write` | Writes file; no shell tools (or shell absent) |
| 3 | Spawn worker `all` | `run_terminal_cmd` background true; start `sleep 30`, poll, kill |
| 4 | Force-broken config (background without companions) in unit test of validator | Error at load **or** auto-repair; never opaque mid-orchestration only |

### Productivity contracts

| # | Case | Expected |
|---|------|----------|
| 5 | Worker completion | Includes runtime `duration_ms` and `tool_calls` |
| 6 | Worker prompt checklist | Mentions artifacts + status |
| 7 | Docs example | Compiles with current spawn API |

### Compatibility

| # | Case | Expected |
|---|------|----------|
| 8 | `general-purpose`, `explore`, `plan`, `manager`, `watcher` | Still build under the same validator |
| 9 | Nested spawn | Still rejected at depth 1 |

---

## 8. Documentation updates

Update when implementing:

| Doc | Change |
|-----|--------|
| `16-subagents.md` | Document `worker` / `manager` / `watcher` if first-class; capability matrix vs tools; spawn failure classes |
| `20-background-tasks.md` | State dependency: background bash requires observe + kill tools on the **same** agent |
| Manager / worker system prompts | Artifact-first completion contract; prefer on-disk partials |
| Skills that hardcode `general-purpose` for implement | Prefer `worker` once P0 lands; note fallback |

---

## 9. Priority and sequencing

| Priority | Item | Rationale |
|----------|------|-----------|
| **P0** | Tool-graph validate + auto-repair for worker bash | Unblocks all implement delegation |
| **P0** | Readable spawn failure taxonomy | Stops cryptic orchestration death |
| **P1** | Honest capability defaults for worker | Role matches tools |
| **P1** | Artifact-first prompt contract | Cuts tokens and transcription errors |
| **P1** | Structured completion payload | Enables managers, watchers, and tools |
| **P2** | Specialist presets, parallelism docs, context guidance | Multiplicative productivity |

**Suggested order:** P0 tool graph → P0 error taxonomy → P1 capability defaults → P1 completion payload (runtime fields) → P1 prompt contract → P2 polish.

---

## 10. Success metrics

| Metric | Target |
|--------|--------|
| Worker spawn success rate for write / execute tasks | ≈ general-purpose (no systematic gap) |
| Production incidents of `enabled_background` requirements failures at spawn | Zero |
| Manager prompts requiring “if worker fails, use GP” workarounds | Eliminated |
| Parent tokens spent re-pasting child outputs | Reduced (qualitative; optional telemetry on spawn prompt sizes) |

---

## 11. Appendix A — Incident detail

**Workflow attempted**

1. Manager follows AGENTS.md quiz instructions.
2. Five parallel `worker` + `read-only` agents answer problems 001–020 … 081–100 → success.
3. `worker` spawn to write `answers.md` → **fail** (tool requirements), twice.
4. Fallback `general-purpose` + `read-write` writes `answers.md` → success.
5. `general-purpose` grades solutions → `results.md` → success.
6. `watcher` verifies 98/100 scoring → pass.

| | |
|---|---|
| **Root cause class** | Tool dependency not satisfied after role / capability toolset assembly |
| **Workaround used** | Avoid `worker` for writes; use `general-purpose` |
| **Desired end state** | Step 3 uses `worker` (`read-write` or `all`) successfully; GP not required |

---

## 12. Appendix B — PR acceptance checklist

Copy into the fix PR description.

### Tool graph and spawn

- [ ] Worker builds with default tools
- [ ] Worker `read-only` builds and cannot edit
- [ ] Worker `read-write` builds and can edit; no broken shell graph
- [ ] Worker `execute` / `all` builds with background shell + observe + kill
- [ ] Auto-include companions **or** auto-disable background (documented)
- [ ] Load-time / diagnostics validation exists
- [ ] Spawn failures include `failure_class` + `failure_hint`
- [ ] Regression test for background-without-companions

### Productivity contracts

- [ ] Worker prompt: artifacts + status completion contract
- [ ] Completion payload includes runtime metrics
- [ ] User-guide updates for background tool dependencies and worker role
- [ ] Manager guidance: artifact-first multi-worker merge pattern

---

## 13. Open questions

| # | Question | Recommendation |
|---|----------|----------------|
| 1 | Should `read-only` ever include non-mutating shell (e.g. `ls`, `git status`), or stay strictly no-shell per user guide? | Stay no-shell for `worker` + `read-only` unless deliberately documented; keep read shell on `explore` if needed |
| 2 | Should companion tools be visible to the model always, or injected as internal-only plumbing when background is used? | Prefer visible companions for parity with general-purpose and simpler debugging |
| 3 | Is `.grok/subagent-artifacts/` the right default, or project-local `tmp/`? | Prefer `.grok/subagent-artifacts/` + gitignore; project `tmp/` only if product policy already uses it |
| 4 | Should managers get an automatic one-shot fallback to `general-purpose` on `spawn_tool_graph` failure? | Prefer fixing P0 over automatic fallback; optional fallback behind a config flag for rollout resilience |

---

*End of enhancement doc.*
