# Codex Context Optimization Implementation Plan

## 1. Background and Problem Statement
- The optimization briefing emphasizes that redundant `sed_range` reads, oversized build logs, and whole-file dumps inflate context usage even when prompt caching performs well, underscoring the need to curb raw bytes injected per turn. 【F:our-docs/CONVERSATION_NOTES.md†L24-L31】
- The package compiled on 2025-09-29 positions the program around suppressing repeat reads, shrinking tool outputs, and stabilizing the prompt prefix without relying on API-key metering. 【F:our-docs/CONVERSATION_NOTES.md†L8-L31】【F:our-docs/README.md†L1-L7】

## 2. Target Outcomes and Success Criteria
- Reduce persisted tool bytes by at least 55%, with expected savings of ~55–60% when line caps, log tailing, and output compaction are enforced. 【F:our-docs/CONVERSATION_NOTES.md†L38-L42】
- Drive duplicate `sed_range` lines per session to ≈0% via overlap suppression, while preventing context overflows and keeping prompt-prefix cache hits stable or improving. 【F:our-docs/CONVERSATION_NOTES.md†L118-L123】
- Maintain developer ergonomics by providing explicit `/relax` escape hatches and a once-per-turn large-slice allowance for small files. 【F:our-docs/policy/context_policy.yaml†L2-L20】【F:our-docs/CONVERSATION_NOTES.md†L134-L138】

## 3. Implementation Phases and Detailed Workstreams

### Phase 1 – Guardrails and Output Budgets (Immediate)
1. **Tool-output caps**
   - Enforce per-call limits of ≤160 lines or ≤8 KB, with per-turn budget of 24 KB; honor language-specific overrides (md/js/ts/rs/kt/tsx/yml up to 200 lines, py/log ≈181, json/txt ≈180) defined in the guidance. 【F:our-docs/CONVERSATION_NOTES.md†L54-L61】【F:our-docs/CONVERSATION_NOTES.md†L82-L85】【F:our-docs/policy/context_policy.yaml†L2-L20】
   - Cap generic command output at 6 KB and `rg` responses at 8 KB, emitting compact JSON summaries rather than full matches.
2. **Command gating**
   - Remove `cat`/`nl` from the safe-command allow list and redirect large file access attempts to the new `read_code` tool with actionable messaging. 【F:our-docs/CONVERSATION_NOTES.md†L54-L61】【F:our-docs/patches/command_safety_patch.diff†L1-L5】
   - Wrap build tooling (`gradle`, `npm`, `pnpm`, `yarn`, `cargo`, `mvn`) with tee-to-disk plus 120-line tail responders and a 10-line failure digest before surfacing output. 【F:our-docs/CONVERSATION_NOTES.md†L54-L61】【F:our-docs/policy/context_policy.yaml†L2-L12】
3. **Repeat-command breaker**
   - Introduce a session-scoped counter (hash or count-min sketch) that aborts on ≥3 identical commands within 120 seconds when no new information is produced, nudging the agent toward narrower queries. 【F:our-docs/policy/context_policy.yaml†L11-L18】【F:our-docs/CONVERSATION_NOTES.md†L82-L86】
4. **Telemetry hooks**
   - Record per-turn metrics for bytes served, lines trimmed, commands blocked, and log-tail invocations to support A/B testing of guardrail efficacy. 【F:our-docs/CONVERSATION_NOTES.md†L76-L79】

### Phase 2 – `read_code` Tool and Overlap Suppression (High Leverage)
1. **Tool registration**
   - Add a strict JSON `read_code` tool in `openai_tools.rs`, exposing parameters for `path`, line ranges, optional symbol targeting, and maximum byte envelope. 【F:our-docs/patches/openai_tools_read_code.diff†L1-L17】【F:our-docs/CONVERSATION_NOTES.md†L62-L65】
2. **Range-serving engine**
   - Implement a session-local interval set keyed by `(path, git_oid)` to track served slices, subtract overlaps on each request, and short-circuit with reference-only responses when the requested region is already in history. 【F:our-docs/CONVERSATION_NOTES.md†L62-L65】【F:our-docs/CONVERSATION_NOTES.md†L89-L95】
3. **Policy enforcement**
   - Enforce per-call and per-turn budgets within the handler, honoring the large-slice exception for small files and `/relax` toggles. 【F:our-docs/policy/context_policy.yaml†L2-L20】
4. **History compaction**
   - Update the transcript compactor to retroactively replace raw byte dumps with `{path,[a,b],oid,chunk_ids}` references so cached prefixes stabilize across turns. 【F:our-docs/CONVERSATION_NOTES.md†L89-L95】

### Phase 3 – Two-pass Context Query Planning (Stabilization)
1. **Planner schema**
   - Require the agent to emit a bounded JSON plan enumerating target files, ranges, symbols, and budgets before executing read operations. 【F:our-docs/CONVERSATION_NOTES.md†L66-L69】
2. **Executor integration**
   - Execute the approved plan via `read_code`, ensuring cumulative reads stay within per-turn limits and reuse existing overlap suppression.
3. **Fallback & UX**
   - Provide structured error messaging when the plan exceeds budgets, directing use of `/relax` or narrower scopes; log plan-vs-actual metrics for experimentation. 【F:our-docs/policy/context_policy.yaml†L15-L18】【F:our-docs/CONVERSATION_NOTES.md†L134-L138】

### Phase 4 – Diff-first Editing with Quiet Patch Acks (Prompt Stability)
1. **Tooling constraints**
   - Make `apply_patch` the sole editing pathway, forbidding shell-based patch streaming. The tool should acknowledge success with concise metadata only, eliminating large patch echoes. 【F:our-docs/CONVERSATION_NOTES.md†L70-L72】
2. **Workflow updates**
   - Document the new editing flow in CLI guidance and ensure downstream analytics treat these acknowledgments as zero-byte for prompt persistence.

### Phase 5 – Context Virtual Memory (Scalability)
1. **Content-addressable index**
   - Build a Git-anchored cache of 4–8 KB line-aware chunks to serve `read_code` slices deterministically and minimize redundant disk I/O. 【F:our-docs/CONVERSATION_NOTES.md†L73-L75】
2. **Dirty-file handling**
   - Hash dirty buffers and track `(size, mtime)` metadata to invalidate affected chunks without blowing away the entire index. 【F:our-docs/CONVERSATION_NOTES.md†L135-L139】

### Phase 6 – Observability and Experimentation (Ongoing)
1. **Metrics surfacing**
   - Emit per-turn counters (bytes, overlap trimmed, blocked reads, repeat-command events) and expose flags `--no-overlap-trim`, `--no-two-pass`, and `--no-build-tail` for controlled rollouts. 【F:our-docs/CONVERSATION_NOTES.md†L76-L79】
2. **A/B harness**
   - Build dashboards comparing persisted bytes, cache-hit rates, latency, and agent success across cohorts to validate each phase before broad release.

## 4. Cross-cutting Considerations and Opinions
- **Developer experience:** Prioritize clear agent messaging when caps trigger; integrate inline guidance suggesting narrower ranges or symbol lookups to avoid frustrating retry loops. 【F:our-docs/CONVERSATION_NOTES.md†L54-L65】
- **Risk mitigation:** Guard against context loss by honoring the large-slice exception and adding forthcoming `read_symbol` helpers; keep full logs on disk with `show log` affordances for manual inspection. 【F:our-docs/CONVERSATION_NOTES.md†L134-L139】
- **Testing strategy:** Stage guardrails behind feature flags, run scenario-based acceptance (large repos, high-churn diffs), and verify token savings with synthetic workloads before rollout.
- **Change management:** Socialize policy defaults and editing constraints with the developer community early, incorporating feedback loops to adjust caps per language if ergonomic friction emerges. 【F:our-docs/policy/context_policy.yaml†L2-L20】

## 5. Execution Timeline (Suggested)
1. **Week 1:** Implement Phase 1 guardrails and telemetry; launch limited beta to quantify immediate byte reductions.
2. **Week 2:** Deliver `read_code`, overlap suppression, and history compaction; validate duplicate-line elimination and adjust caps as needed.
3. **Week 3:** Introduce planner turn with opt-in flag; collect usability feedback and iterate on error messaging.
4. **Week 4:** Enforce diff-first editing; finalize documentation and developer training materials.
5. **Weeks 5–6:** Develop and integrate CVM cache; begin broader rollout backed by observability dashboards.
6. **Beyond:** Continue metrics-driven tuning, refine per-extension caps, and explore adaptive budgeting informed by session-level behavior analytics. 【F:our-docs/CONVERSATION_NOTES.md†L143-L147】

## 6. Definition of Done
- Guardrails, `read_code`, planner, and patch-ack updates deployed with feature flags and validated savings meeting ≥55% persisted-byte reduction target. 【F:our-docs/patches/IMPLEMENTATION_PLAN.md†L4-L5】【F:our-docs/CONVERSATION_NOTES.md†L118-L123】
- Observability suite exposes overlap, budget, and repeat-command metrics; dashboards confirm sustained prompt-prefix stability.
- Documentation (policy files, developer guides) aligns with enforced limits and provides clear override instructions.

---
Prepared as KSP-PLAN.md to capture the actionable roadmap and personal guidance for implementing the Codex context optimization program.
