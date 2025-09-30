# Codex Context Optimization — Codex-Ready Action Plan

The action plan is organized into intensive Codex execution sessions. Each session can be completed in a single, focused push with room for iteration, testing, and guardrail validation. Sessions list prerequisites, deliverables, and embedded cautions so future Codex agents can select the next actionable block with minimal re-planning.

## Global Objectives and Guardrails
- Hit ≥55% reduction in persisted tool bytes by enforcing read caps, trimming logs, and compacting transcripts. 【F:our-docs/CONVERSATION_NOTES.md†L24-L42】
- Drive duplicate `sed_range` reads to ~0% through overlap suppression without hurting prompt cache hit rates. 【F:our-docs/CONVERSATION_NOTES.md†L89-L123】
- Preserve ergonomics via `/relax` escape hatches and a once-per-turn large-slice allowance for small files. 【F:our-docs/policy/context_policy.yaml†L2-L20】【F:our-docs/CONVERSATION_NOTES.md†L134-L138】

---

## Session A — Guardrails & Output Budget Hardening
- **Scope:** Finalize immediate guardrails, turn budgeting, and telemetry introduced in prior work so they withstand production load.
- **Entry Conditions:** Existing clamps and command gating are merged but need verification, polish, and expanded coverage. 【F:codex-rs/core/src/exec.rs†L26-L438】【F:codex-rs/core/src/state/turn.rs†L1-L218】
- **Exit Criteria:** All guardrails emit actionable messages, persist metrics, and reject bypass attempts while keeping `/relax` functionality ready for later phases.
- **Subtasks:**
  1. Audit truncation messaging for exec, `rg`, and per-turn budgets; ensure consistent phrasing and small-file exception callouts. 【F:codex-rs/core/src/codex.rs†L520-L1050】
  2. Validate repeat-command breaker behavior under concurrent commands; add regression tests for hashed output collision handling. 【F:codex-rs/core/src/state/session.rs†L1-L214】
  3. Extend telemetry payloads with command context for upcoming dashboards (bytes served/trimmed, truncated outputs, breaker trips). 【F:codex-rs/core/src/tasks/mod.rs†L20-L120】
- **Warnings:** Avoid widening truncation caps; optimize messaging instead. Respect existing safe-command denylist changes (no `cat`/`nl`). 【F:core/src/command_safety/is_safe_command.rs†L17-L198】
- **Dependencies:** None; this session establishes the hardened baseline for all subsequent work.

---

## Session B — `read_code` Overlap Suppression & History Compaction
- **Scope:** Finish `read_code` session accounting and transcript compaction so repeated slices are eliminated and caching stabilizes.
- **Entry Conditions:** Tool registration and per-call caps ship; per-turn accounting and overlap tracking remain unimplemented. 【F:core/src/tool_read_code.rs†L1-L249】
- **Exit Criteria:** Transcript history stores `{path,[a,b],oid,chunk_ids}` references, per-turn budgets exist, and `/relax` toggles integrate cleanly.
- **Subtasks:**
  1. Implement `(path, git_oid)` interval tracking with interval trees to block redundant reads within the same turn; add targeted tests. 
  2. Hook range-serving engine into turn budget accounting; surface concise notices when limits hit and allow one `/relax` override per turn. 【F:our-docs/policy/context_policy.yaml†L15-L18】
  3. Update transcript compactor to retroactively replace raw slices with structured references; verify prompt cache stability in prompt-caching suite. 【F:core/tests/suite/prompt_caching.rs†L210-L235】【F:our-docs/CONVERSATION_NOTES.md†L89-L95】
- **Warnings:** Maintain small-file exception semantics; ensure `/relax` escape is logged for observability. Avoid introducing new IO-heavy primitives.
- **Dependencies:** Session A complete (telemetry fields leveraged for compaction validation).

---

## Session C — Two-pass Context Query Planning
- **Scope:** Require Codex to declare read intent before execution, validating budgets and reuse of overlap suppression.
- **Entry Conditions:** Overlap tracking and transcript compaction live. Planner scaffolding absent.
- **Exit Criteria:** Agent emits bounded JSON plan, executor validates budgets, UX handles plan rejection gracefully.
- **Subtasks:**
  1. Define planner schema (files, ranges, symbols, byte budget) and integrate into OpenAI tool set. 【F:our-docs/CONVERSATION_NOTES.md†L66-L69】
  2. Implement plan validator that enforces per-turn limits, reuses overlap data, and halts execution when budgets would be exceeded.
  3. Add fallback UX with actionable errors, `/relax` guidance, and plan-vs-actual metrics instrumentation. 【F:our-docs/CONVERSATION_NOTES.md†L134-L138】
- **Warnings:** Ensure planner cannot bypass small-file exception misuse; reject recursive planner invocations.
- **Dependencies:** Sessions A & B deliver guardrails and overlap primitives used here.

---

## Session D — Diff-first Editing & Quiet Patch Acknowledgments
- **Scope:** Force editing through `apply_patch`, mute patch echoes, and update CLI UX to reflect new workflow.
- **Entry Conditions:** Shell-based patch streaming still possible; acknowledgments echo full patches.
- **Exit Criteria:** `apply_patch` is sole edit tool, responses include minimal metadata, and CLI docs steer users appropriately.
- **Subtasks:**
  1. Enforce policy: block shell-based patch commands and ensure tooling errors reference `apply_patch` guidance. 【F:our-docs/CONVERSATION_NOTES.md†L70-L72】
  2. Modify `apply_patch` response path to emit concise success payloads (no diff body) while preserving failure diagnostics.
  3. Update CLI developer guidance to explain diff-first editing and quiet acknowledgments; tag documentation for Codex consumers only.
- **Warnings:** Preserve debugging affordances—ensure logs remain accessible via `show log`. 【F:our-docs/CONVERSATION_NOTES.md†L134-L139】
- **Dependencies:** Sessions A–C deliver context-limiting policies that editing UX must respect.

---

## Session E — Context Virtual Memory (CVM) Cache
- **Scope:** Add Git-anchored, content-addressed chunk cache serving deterministic `read_code` responses with dirty-file invalidation.
- **Entry Conditions:** Overlap suppression, planner, and diff-first editing in place. No chunk cache yet.
- **Exit Criteria:** CVM cache serves 4–8 KB line-aware chunks, handles dirty buffers, and surfaces metrics for cache hits/misses.
- **Subtasks:**
  1. Design content-addressable index keyed by `(repo_oid, path, chunk_range)`; ensure deterministic slicing for prompt cache synergy. 【F:our-docs/CONVERSATION_NOTES.md†L73-L75】
  2. Implement dirty-file tracking via hash + `(size, mtime)` metadata with targeted invalidation routines. 【F:our-docs/CONVERSATION_NOTES.md†L135-L139】
  3. Instrument cache hit/miss counters and wire into telemetry to support rollout gating.
- **Warnings:** Avoid large upfront indexing; build lazily on-demand to respect per-turn budgets.
- **Dependencies:** Sessions B & C supply overlap metadata consumed by CVM.

---

## Session F — Observability & Experimentation Rollout
- **Scope:** Expose collected metrics, ship rollout flags, and create A/B harness to validate savings before broad launch.
- **Entry Conditions:** Telemetry streams exist but lack surfaced dashboards and gating flags.
- **Exit Criteria:** Operators can toggle overlap suppression, two-pass planner, build-tail trims, and observe impact across cohorts.
- **Subtasks:**
  1. Wire telemetry fields into surfaced metrics (CLI, dashboards, or log sinks) including bytes served/trimmed, overlap suppressed, repeat breaker trips. 【F:our-docs/CONVERSATION_NOTES.md†L76-L79】
  2. Implement runtime flags `--no-overlap-trim`, `--no-two-pass`, `--no-build-tail` and ensure session metadata records flag usage.
  3. Build lightweight A/B harness summarizing persisted bytes, cache-hit rates, latency, and agent success to validate phased rollouts.
- **Warnings:** Keep dashboards lightweight and developer-facing; defer business reporting.
- **Dependencies:** Sessions A–E generate the metrics that F visualizes.

---

## Ongoing Quality & Definition of Done
- **Testing Strategy:** Feature-flag each major control, create synthetic large-repo scenarios, and verify prompt-caching suite stability. 【F:our-docs/patches/IMPLEMENTATION_PLAN.md†L4-L5】
- **DoD:** Guardrails, `read_code`, planner, patch acknowledgments, CVM cache, and observability must collectively hit ≥55% persisted-byte reduction while keeping prompt prefixes stable. 【F:our-docs/CONVERSATION_NOTES.md†L118-L147】
- **Documentation:** Limit updates to Codex-facing developer guidance that describe new tool behavior and escape hatches; avoid broader business process docs.

Prepared for Codex agents as a focused execution roadmap based solely on the current KSP inputs.
