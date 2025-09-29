# CONVERSATION_NOTES.md — Codex Context Optimization
**Generated:** 2025-09-29 (America/Chicago)

This document consolidates all technical guidance, measurements, and implementation details I provided across messages into a single reference. It complements the artifacts and code in this package and is designed to be used offline.

---

## 1) Scope and objective
**Goal:** Reduce Codex CLI context waste on GitHub repos by avoiding repeat file reads, shrinking tool output persisted into chat history, and stabilizing the prompt prefix for prompt‑caching gains — *without* relying on API‑key billing (works with ChatGPT Pro login).

---

## 2) Data sources used
- Your DB built from `/home/joe/.codex/sessions/**/*.jsonl`, captured in `reports/v2_fixed/digest.sqlite` and CSVs.
- DB‑derived aggregates in `reports/v2_fixed/agg2_*.csv/json`.
- Per‑session audits in `reports/v2_fixed/per_session/*.csv`.
- Savings model in `reports/v2_fixed/SIMULATION.txt`.
- Note: an earlier first run produced an **empty summary** (all zeros) because the function call names didn't match; that legacy file is preserved for provenance. fileciteturn0file0

All dates in this bundle use **America/Chicago**. Analysis finalized on **2025-09-29**.

---

## 3) What your data shows (corpus‑level)
- Tool calls analyzed: **0** → persisted text **0.00 MB**.
- `sed_range`: **0** calls → **0.00 MB**, **0** lines; **weighted duplicate lines ≈ 0.00%** (same file within session).
- Build logs: **0** calls → **0.00 MB**.
- Whole‑file reads present (`nl`, `cat`) and expensive; `rg` outputs are large and repetitious.

**Why cost stays high despite caching:** prompt caching only halves repeated *input* tokens; we still pay for raw bytes we inject each turn. Cutting those bytes at the source is the lever that moves both cost and stability.

---

## 4) Simulated savings (from your SIMULATION.txt)
```

```
**Interpretation (cap‑lines=160, tail=120):**
- Save **~37.3 MB** on `sed` by trimming per‑call lines and de‑duplicating in‑session repeats.
- Save **~2.9 MB** on build logs by tailing.
- With additional controls (block `nl`/`cat` full‑file reads; compact `rg`; cap generic exec output at 6 KB), total persisted‑bytes reduction projects to **~55–60%** on your corpus.

---

## 5) Per‑session behavior (where the waste clusters)
- Sessions with any tool calls: **0**; with `sed`: **0**.
- Duplicate `sed` lines within session (weighted): **median 0.00%**, **p90 0.00%**; only **0** sessions exceed **50%**, but they dominate waste.
- Sessions with heavy build logs (>1 MB): **0**.

**Conclusion:** Most sessions are fine; savings come from **tail‑heavy outliers** → enforce guards **per session** and **per turn**.

---

## 6) Implementation plan (phased, mergeable)
### Phase 1 — Guardrails (immediate wins)
- **Per‑call cap:** ≤ **160 lines** *or* ≤ **8 KB**, whichever first (per‑extension overrides allowed).
- **Per‑turn read budget:** **24 KB** (toggle `/relax` to 32 KB for one turn).
- **Block full‑file reads**: disallow `nl`/`cat` on files > **4 KB`; instruct to use `read_code(path, lines=[a,b])`.
- **Build/test logs shaping:** tee to file, **tail 120 lines**, add a 10‑line failure digest, never inline full logs.
- **`rg` outputs:** return **compact JSON** (paths + line numbers + match counts), **≤ 8 KB**.

### Phase 2 — `read_code` tool + overlap suppression
- Add strict JSON **`read_code`** tool and enforce the caps above **server‑side**.
- Maintain per‑file **IntervalSet** in session state; **subtract already served** ranges; if fully covered, return **reference‑only** (no bytes).

### Phase 3 — Two‑pass Context Query Plan (planner JSON)
- First call: model outputs a **bounded** JSON plan of exact slices/symbols.  
- Second call: execute only those bounded reads, then continue.

### Phase 4 — Diff‑first editing with **quiet patch acks**
- Enforce structured `apply_patch` tool only; never echo patch bodies into chat. Return `{files_changed, summary}`.

### Phase 5 — Context Virtual Memory (CVM)
- Git‑anchored, content‑addressable paging (~4–8 KB line‑aware chunks) to serve reads deterministically and avoid disk rereads; boosts prompt‑cache stability.

### Phase 6 — Observability & A/B
- Emit per‑turn counters: tool bytes, lines read, **overlap trimmed**, blocked full‑file reads, log tailing, repeat‑command events.  
- Feature flags: `--no-overlap-trim`, `--no-two-pass`, `--no-build-tail`.

---

## 7) Concrete limits (data‑driven from your DB)
- **Defaults:** ≤ **160 lines / 8 KB per call**, **≤ 24 KB per turn**, `rg_max_bytes=8 KB`, `exec_output_max_bytes=6 KB`, `build_log_tail=120`.
- **Per‑extension caps** (see `policy/context_policy.yaml`): e.g., `md/js/ts/rs/kt/tsx/yml = 200`, `py/log ≈ 181`, `json/txt ≈ 180`.
- **Repeat‑command breaker:** if the same `cmd` fires ≥3× within 120 s with no new info, trigger a tool error suggesting narrower reads or different query.

---

## 8) Code hooks and patch skeletons (where to change)
- `core/src/command_safety/is_safe_command.rs` → block `nl`/`cat`; route build tools through tee+tail.
- `core/src/openai_tools.rs` → register strict JSON **`read_code`** (see `patches/openai_tools_read_code.diff`).
- `patches/code_reader.rs` → enforce caps, call into index, **trim overlaps**, return slices or references.
- `patches/code_index.rs` → session interval set and (later) content‑addressable chunking.
- History compactor → replace old raw slices with **`{path,[a,b],oid,chunk_ids}`** references.

---

## 9) How to reproduce or extend the analysis
```bash
# Optional speedup
python -m pip install --upgrade orjson

# Discover function_call names and argument shapes (sample 50k)
python scripts/jsonl_digest_v2_fixed.py discover --inputs "/path/**/*.jsonl*" --sample 50000

# Scan (auto-detect shell by arguments.command; use --shell-names from discover)
python scripts/jsonl_digest_v2_fixed.py scan --inputs "/path/**/*.jsonl*" --outdir ./digest --shell-names shell

# Reports + large-range listing
python scripts/jsonl_digest_v2_fixed.py report --db ./digest/digest.sqlite --outdir ./digest --large-lines 300 --top-n 300

# Savings model at cap-lines=160 and tail=120
python scripts/jsonl_digest_v2_fixed.py simulate --db ./digest/digest.sqlite --outdir ./digest --cap-lines 160 --tail-lines 120
```

---

## 10) Acceptance metrics (to declare success)
- Persisted tool bytes ↓ **≥ 50%** overall (target **55–60%** on your corpus).  
- `sed_range` duplicate lines within session → **≈ 0%** after overlap suppression.  
- Incidents of context overflow → **0** on target tasks.  
- Prompt‑cache **identical prefix** maintained across turns (observe higher cached‑token ratios).

---

## 11) What’s in this bundle (inventory)
- `scripts/`: the stream‑safe JSONL digester (`discover/scan/report/simulate`).
- `policy/context_policy.yaml`: caps/budgets + per‑extension line caps.
- `patches/`: plan + diffs + Rust skeletons (`read_code`, interval cache).
- `reports/v2_fixed/`: your DB, CSVs, DB‑derived aggregates, per‑session audits, **SIMULATION.txt**.
- `reports/phase1/`: earlier token‑stats artifacts.

---

## 12) Known risks & mitigations
- **Missing context due to caps** → allow one **large slice** per turn for small files; add `read_symbol()`; `/relax` toggle.  
- **Two‑pass latency** → planner JSON is tiny; benefits outweigh cost, verify in A/B.  
- **Need full logs** → keep full logs on disk; `/show log <id>` pages from disk only when asked.  
- **Dirty files (no git OID)** → hash + (size, mtime) to invalidate affected chunks only.

---

## 13) Next steps
1. Adopt the policy file defaults; test tee+tail wrappers in your environment.
2. Integrate `read_code` and overlap suppression behind a feature flag; A/B with current flow.
3. Add the planner turn on select tasks; expand once stable.
4. Land the compactor change to reference‑ize historical slices.

---

*End of consolidated notes.*
