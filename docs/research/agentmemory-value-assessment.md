# AgentMemory Value Assessment for PICE

*PICE Research Library - architecture assessment for future memory work.*

## Executive decision

AgentMemory is useful prior art for PICE, but it should not be adopted as a direct runtime dependency in the first PICE memory integration.

The value is in its patterns: durable observations, token-budgeted context assembly, search and recall filtering, replay/import guardrails, delete/governance flows, commit/session linking, and plugin manifest tests. The first PICE-native path should be smaller: opt-in, summary-only memory that PICE records after successful workflow phases and reads only into approved workflow prompts.

The reason to reject a direct runtime dependency is architectural, not quality-based. PICE already owns the workflow lifecycle, verification manifests, SQLite metrics and audit trails, provider JSON-RPC, daemon RPC, Stack Loops context isolation, and evaluator prompts. Importing AgentMemory as a server, MCP runtime, hook layer, or provider-side memory caller would create a second source of truth and could weaken the exact isolation guarantees PICE is built around.

## Inspected snapshot

Planning snapshot recorded:

- Repository: [rohitg00/agentmemory](https://github.com/rohitg00/agentmemory)
- Snapshot commit recorded in the approved plan: `3a24c32` (`fix(viewer): prevent IME composition interruption in search inputs (#517)`)
- Package: [`@agentmemory/agentmemory`](https://github.com/rohitg00/agentmemory/blob/main/package.json)
- Package version observed: `0.9.20`
- License observed: `Apache-2.0`
- Date of this assessment: 2026-05-19

Spot checks on `main` during execution still showed `package.json` at version `0.9.20`, license `Apache-2.0`, and a changelog entry for `0.9.20` that reverted the Codex `Stop` to session-end chain. Future AgentMemory changes may alter implementation details, benchmark claims, hook behavior, and risk tradeoffs; this document should be refreshed before any implementation plan relies on a newer release.

Primary source references:

- [package.json](https://github.com/rohitg00/agentmemory/blob/main/package.json)
- [CHANGELOG.md](https://github.com/rohitg00/agentmemory/blob/main/CHANGELOG.md)
- [context.ts](https://github.com/rohitg00/agentmemory/blob/main/src/functions/context.ts)
- [observe.ts](https://github.com/rohitg00/agentmemory/blob/main/src/functions/observe.ts)
- [search.ts](https://github.com/rohitg00/agentmemory/blob/main/src/functions/search.ts)
- [replay.ts](https://github.com/rohitg00/agentmemory/blob/main/src/functions/replay.ts)
- [governance.ts](https://github.com/rohitg00/agentmemory/blob/main/src/functions/governance.ts)
- [hooks.codex.json](https://github.com/rohitg00/agentmemory/blob/main/plugin/hooks/hooks.codex.json)
- [codex-plugin.test.ts](https://github.com/rohitg00/agentmemory/blob/main/test/codex-plugin.test.ts)

PICE boundary references used for the conclusions below:

- [`AGENTS.md`](../../AGENTS.md)
- [`PRDv2.md`](../../PRDv2.md)
- [daemon rules](../../.codex/rules/daemon.md)
- [Stack Loops rules](../../.codex/rules/stack-loops.md)
- [provider protocol rules](../../.codex/rules/protocol.md)
- [provider protocol docs](../providers/protocol.md)

## Reusable patterns

Token-budgeted context assembly:

Source anchors: `context.ts`; PICE prompt-boundary references: provider protocol rules and provider protocol docs.

AgentMemory's `mem::context` builds candidate blocks from pinned slots, project profile, lessons, session summaries, and selected observations, estimates token cost, sorts by recency/relevance, and drops blocks that exceed budget. PICE should adapt the principle, not the code: assemble a bounded, precomputed memory brief outside prompt builders, then pass that brief into eligible workflow prompts as inert context.

Durable observations:

Source anchors: `observe.ts`; PICE lifecycle references: `AGENTS.md`, daemon rules, and Stack Loops rules.

`mem::observe` validates hook payloads, sanitizes raw data, deduplicates repeated tool events, enforces per-session limits, records observations, and emits stream updates. PICE can reuse the shape of this pipeline for future `SessionMemoryRecorder` design: validation first, redaction before persistence, bounded writes, append-only auditability, and no dependence on hook events as the authoritative workflow lifecycle.

Search and recall filtering:

Source anchors: `search.ts`; PICE state/isolation references: daemon rules and Stack Loops rules.

AgentMemory combines BM25-style indexing, optional vector indexing, project/cwd filters, limit caps, compact output modes, and guarded vector writes. PICE should defer vector/graph/rerank implementation, but it can borrow the practical constraints: filter by project hash, layer, feature id, plan path, and phase before anything reaches a model; cap recalled items and token budget; make missing embedding providers a non-fatal recall degradation.

Replay/import guardrails:

Source anchors: `replay.ts`; PICE privacy/isolation references: `AGENTS.md` and Stack Loops rules.

AgentMemory's replay/import surface is valuable because it treats older JSONL transcripts as untrusted input, not just convenient history. The pattern PICE should reuse is explicit import contracts: reject sensitive paths, skip malformed lines without aborting a batch, record import provenance, and keep replay/import out of first-pass runtime memory.

Governance and delete operations:

Source anchors: `governance.ts`; PICE audit references: daemon rules and `PRDv2.md`.

The governance/delete surface is a necessary companion to memory. PICE should not add durable memory without an operator-visible delete or prune path, retention policy, and audit semantics. This matters because coding-session memory can contain source excerpts, prompts, tool output, file paths, and operational context.

Commit/session linking:

Source anchors: `CHANGELOG.md`; PICE manifest/metrics references: daemon rules and Stack Loops rules.

AgentMemory's changelog records session-to-commit linking through a commit namespace and MCP/REST lookup surfaces. PICE has a natural analogue: plan path, feature id, run id, manifest path, evaluation id, and final commit. This is worth adapting later, but it must be PICE-owned and tied to manifests and metrics rather than an external memory runtime.

Plugin manifest tests:

Source anchors: `hooks.codex.json` and `codex-plugin.test.ts`; PICE template/provider references: provider protocol rules and provider protocol docs.

AgentMemory tests its Codex plugin manifest and hook surface. PICE should use that as prior art for any future generated hook or plugin output: assert exact hook lists, command paths, MCP wiring, and disabled-by-default behavior in tests before shipping templates or install helpers.

## Rejected first-pass paths

Reject direct runtime dependency:

Source anchors: `package.json`; PICE ownership references: `AGENTS.md`, daemon rules, Stack Loops rules, and provider protocol rules.

Do not add `@agentmemory/agentmemory`, `iii-sdk`, AgentMemory MCP config, AgentMemory server startup, embeddings packages, vector databases, or new lockfile entries in the first PICE memory plan. That would make PICE's daemon lifecycle depend on a second runtime with its own server, hooks, state, and release cadence.

Reject AgentMemory server or MCP as PICE source of truth:

Source anchors: `package.json`, `governance.ts`, and `search.ts`; PICE source-of-truth references: daemon rules and Stack Loops rules.

PICE's verification manifest under `~/.pice/state/{project_hash}/{feature-id}.manifest.json` remains the source of truth for current evaluation state. SQLite remains metrics and audit storage. A memory system can be advisory context, but it must never become the authoritative record for plan approval, layer status, review gates, evaluation verdicts, or release readiness.

Reject provider-side memory calls:

Source anchors: `context.ts`, `search.ts`, and provider protocol docs; PICE protocol references: provider protocol rules.

Providers should implement the provider JSON-RPC protocol and report workflow/evaluation events. They should not independently call memory tools, mutate memory, or retrieve context. Provider-side memory calls would bypass daemon policy, complicate redaction, and risk making Claude/Codex behavior diverge in ways PICE cannot audit.

Reject hook-driven lifecycle:

Source anchors: `CHANGELOG.md` and `hooks.codex.json`; PICE lifecycle references: daemon rules and `AGENTS.md`.

AgentMemory's `0.9.20` changelog is the key warning: Codex `Stop` fired before the full conversation was truly finished, so chaining session-end from `Stop` marked sessions complete too early. PICE must keep authoritative lifecycle in daemon/provider-session state. Codex Stop can be an input signal at most; it is not a session-end authority.

Reject raw prompt/tool capture by default:

Source anchors: `observe.ts` and `replay.ts`; PICE privacy/isolation references: `AGENTS.md` and Stack Loops rules.

Raw prompt and tool-output capture are high-risk because they may contain secrets, proprietary code, credentials, incident data, customer material, or private user notes. First-pass PICE memory should store summaries and bounded structured metadata only. Raw capture, replay/import, and transcript retention require a later explicit contract.

Reject evaluator/review prompt injection:

Source anchors: `context.ts` and `search.ts`; PICE evaluator-isolation references: `AGENTS.md`, Stack Loops rules, and provider protocol rules.

The first runtime memory plan must exclude `review`, `evaluate`, adversarial review, and `commit`. Evaluators and review prompts must not receive recalled memory unless a future contract explicitly approves that change and proves it preserves evaluator isolation. `commit` must stay tied to staged diff only.

Reject immediate vector/graph/rerank implementation:

Source anchors: `search.ts`; PICE implementation-scope references: `PRDv2.md`, daemon rules, and Stack Loops rules.

AgentMemory's retrieval stack is interesting, but PICE should not start with embeddings, graph paths, rerankers, or local model dependencies. Start with simple summaries, deterministic filters, and a small token budget. Add retrieval complexity only after metrics prove it improves outcomes without polluting evaluation.

## PICE-native future options

Option 1: `.pice/learnings.md` or `.pice/knowledge.md`

This is the most transparent first step. It is human-readable, reviewable in git, easy to prune, and aligned with the existing self-evolving verification research. It works best for durable project lessons, recurring validation gotchas, and conventions. Weaknesses: concurrent writes need care; sensitive content can be accidentally committed; structured queries are limited.

Option 2: `.pice/metrics.db`

SQLite is already the local engine for evaluations, criteria scores, loop events, pass events, layer runs, seam findings, and gate decisions. It is a good future home for structured memory metadata, access logs, retention metadata, and source-to-run links. Weaknesses: metrics is not the manifest source of truth; dumping natural-language memory into metrics tables would blur analytics and context unless schema boundaries are explicit.

Option 3: project-hashed state under `~/.pice/state`

The project-hashed state namespace is the right place for per-project, non-committed operational state that must avoid cross-repo collisions. It is a good fit for ephemeral or private memory summaries, import staging, and cached recall artifacts. Weaknesses: less visible in git review; needs pruning/export tooling; must never compete with manifest files as the source of truth.

## Storage comparison

| Option | Best use | Strength | Risk |
| --- | --- | --- | --- |
| `.pice/learnings.md` or `.pice/knowledge.md` | Human-readable project lessons | Transparent, auditable, simple | May leak sensitive context if committed blindly |
| `.pice/metrics.db` | Structured events, links, access/retention metadata | Queryable, already local, WAL-backed | Can blur metrics and memory if schema is loose |
| `~/.pice/state/{project_hash}` | Private per-project memory and caches | Avoids cross-repo collision, not committed | Less visible; needs lifecycle tooling |

Recommended first implementation path: summary-only `.pice/learnings.md` for explicit project lessons, plus optional private state under `~/.pice/state/{project_hash}` for non-committed summaries. Use `.pice/metrics.db` only for metadata and audit links until a separate storage contract approves natural-language memory tables.

## Prompt and evaluator isolation boundary

Memory is advisory context, not authority. Plans, contracts, manifests, review gates, and metrics remain authoritative PICE records.

Allowed first-pass consumers:

- `prime`: may summarize existing project lessons as orientation context.
- `plan`: may receive memory only when the user opts in and the memory is marked as project guidance, not source requirements.
- `execute`: may receive approved project lessons that are relevant to the plan scope.
- `handoff`: may write a summary-only memory record after user-visible state is captured.

Excluded first-pass consumers:

- `evaluate`: must not receive recalled memory.
- Adversarial review: must not receive recalled memory.
- `review`: must not receive recalled memory.
- `commit`: must not receive recalled memory; commit generation must remain staged-diff scoped.

Future design names worth reserving in a separate plan:

- `SessionMemoryRecorder`: a daemon-owned recorder for summary-only memory writes at approved lifecycle points.
- `SessionRunContext`: a daemon-owned context object that carries feature id, plan trace, run id, provider name, project hash, and allowed memory policy.

Prompt builders should stay storage-free. They should receive precomputed context assembled by daemon policy. A prompt builder opening SQLite, reading `~/.pice/state`, calling MCP, or running retrieval directly would hide policy inside presentation code and make evaluation isolation harder to prove.

## Privacy and retention risks

Raw prompt capture:

User prompts can include private intent, credentials, customer details, source excerpts, issue links, or incident context. Raw prompt storage is out of scope for first-pass PICE memory and must require a future approval gate.

Tool-output capture:

Tool output can include secrets, database rows, stack traces, deployment logs, private filenames, and proprietary source. First-pass memory should store summaries that have already passed through redaction. Any future tool-output capture needs explicit redaction tests and retention controls.

Replay/import:

Replay/import can ingest years of sensitive history in one command. It needs a separate contract with path allowlists, symlink rejection, sensitive-name rejection, malformed-line handling, import provenance, dry-run mode, and delete/prune operations. It should not be hidden inside the first memory implementation.

Retention:

Memory must have deletion and pruning from the first runtime implementation. The default should be conservative, opt-in, and visible. Retention policy should distinguish committed project lessons, private state summaries, metrics metadata, and imported transcripts.

Model contamination:

The main PICE-specific risk is evaluator contamination. Recalled memory can smuggle implementation rationale, prior evaluator findings, sibling layer facts, or stale assumptions into a prompt that must remain isolated. Until a future contract proves otherwise, evaluator, adversarial review, and code review contexts must not receive recalled memory.

## Recommended next plan

Create a separate implementation plan for "PICE-native summary memory v1" with these boundaries:

- Add no AgentMemory runtime dependency.
- Store summary-only project lessons.
- Make the feature opt-in.
- Exclude `review`, `evaluate`, adversarial review, and `commit`.
- Keep `commit` staged-diff only.
- Define `SessionMemoryRecorder` and `SessionRunContext` before touching workflow sessions.
- Keep prompt builders storage-free by passing precomputed context.
- Preserve manifest source-of-truth semantics.
- Use `.pice/learnings.md` or `.pice/knowledge.md` for human-readable lessons.
- Use `.pice/metrics.db` only for metadata unless a separate schema contract approves memory tables.
- Use `~/.pice/state/{project_hash}` only for private, non-committed summaries or caches.
- Treat replay/import, raw prompt capture, tool-output capture, commit/session linking, MCP exposure, vector search, graph search, and reranking as later separately contracted features.

The next plan should include explicit validation that no memory text reaches `evaluate/create`, adversarial evaluator prompts, `pice review`, or `pice commit`.
