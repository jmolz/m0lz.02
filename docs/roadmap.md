# PICE Roadmap

This document outlines the vision for PICE beyond v0.1. The core loop — Plan → Implement → Contract-Evaluate — is stable and shipping. What follows is where it's going, grounded in empirical research and mathematical foundations.

Feedback welcome in [Discussions](https://github.com/jmolz/pice-framework/discussions).

-----

## v0.1 — Current Release ✅

Structured AI coding workflow orchestrator with dual-model adversarial evaluation.

- Plan, Implement, Contract-Evaluate lifecycle
- Dual-model adversarial evaluation (Claude + GPT, context-isolated)
- Tiered evaluation (Tier 1/2/3 scaled to change significance)
- Provider architecture (Rust core + TypeScript providers over JSON-RPC)
- SQLite metrics engine for quality tracking
- Template scaffolding for new and existing projects
- 217 tests passing

-----

## The Seam Problem

Every version of PICE after v0.1 is shaped by a single observation: **software breaks at the boundaries between components, not inside them.**

This is not opinion. It is empirically documented across every major postmortem database in the industry:

- **Google SRE** analyzed thousands of postmortems (2010–2017) and found 68% of all outages are triggered by configuration and binary pushes at integration points. Software bugs cause 41.35% of root causes, development process failures 20.23%, and complex system behaviors 16.90%. 82% of configuration-related triggers stem from manual oversight at boundaries. ([Source: Google SRE Workbook, Postmortem Analysis](https://sre.google/workbook/postmortem-analysis/))
- **Adyen** (ICSE-SEIP 2018) studied 2.43 million API error responses and identified 11 general causes of integration faults, dominated by invalid/missing request data and third-party integration failures. ([Source: Adyen/ICSE 2018](https://dl.acm.org/doi/10.1145/3183519.3183537))
- **Gregor et al.** (ICST 2025, TU Munich/Siemens) built a 23-category taxonomy of integration-relevant faults for microservice testing. 21 of 23 fault categories were experienced by over 50% of surveyed practitioners — these aren't edge cases; they're systemic realities.
- **AWS US-EAST-1** (October 2025) experienced a DNS race condition in DynamoDB's management system that cascaded across EC2, Lambda, and NLB for 14+ hours — a single seam failure propagating through an entire architecture.

Michael Nygard's canonical observation remains true three decades after the Deutsch fallacies: **integration points are "the number-one killer of systems."**

### AI agents make the seam problem worse, not better

AI coding agents optimize for local correctness within components but systematically fail at integration boundaries. The empirical evidence is now substantial:

- **SWE-Bench Pro** (Scale AI, September 2025) tested top models on 1,865 multi-file problems. Models scoring >70% on single-file SWE-Bench Verified achieved **only ~23%** when changes spanned multiple files averaging 4.1 files and 107.4 lines. ([Source: arXiv:2509.16941](https://arxiv.org/abs/2509.16941))
- **SWE-CI** (March 2026) tested agents on continuous integration maintenance. The zero-regression rate was **below 0.25** for most models — agents broke existing behavior in over 75% of maintenance iterations.
- **CodeRabbit** analyzed 33,596 agent-authored PRs and found AI-generated code contains **1.7x more issues**, with logic/correctness errors 1.75x more common, business logic errors >2x, and security vulnerabilities 1.5–2x higher.
- **GitClear** (211 million lines analyzed) found code duplication rose **8x** in 2024 vs. pre-AI baseline, while refactoring collapsed from 24% to below 10%.
- **Harvard's compositional specification research** (DafnyComp benchmark) identified the root cause: LLMs handle local specs but fail under composition — they optimize for local correctness, not global structural integrity.

### The tooling gap: everyone built for components, nobody built for seams

The entire verification tooling landscape is siloed:

| Tool category | What it checks | What it misses |
| --- | --- | --- |
| Contract testing (Pact, Specmatic) | Structural compatibility — data shapes match | Behavioral correctness, ordering, timing, capacity |
| Architecture analysis (ArchUnit, jQAssistant) | Static dependency rules | Runtime coupling, temporal dependencies, config drift |
| Formal methods (session types, TLA+) | Mathematical protocol guarantees | Zero adoption in production microservices |
| AI coding agents (Claude Code, Cursor) | Local code correctness | Integration assumptions, cross-component contracts |

**No tool in existence can automatically discover that Component A assumes something about Component B that Component B doesn't guarantee.** Garlan et al. identified this as the core problem of "architectural mismatch" in 1995 but proposed better documentation — not automated detection. Thirty years later, the gap remains open.

PICE v0.2+ targets this gap. Every other AI coding tool optimizes for generating correct code within components. **PICE is the first to systematically verify that the spaces between components actually hold.**

> **Expanded research:** [The Seam Blindspot: Where Software Really Breaks and What No One Is Building to Fix It](research/seam-blindspot.md) — full 23-category failure taxonomy, tool capability gap analysis, cross-domain verification approaches from hardware and formal methods.

-----

## v0.2 — Stack Loops *(novel concept, coined in PICE)*

**The problem:** AI-assisted development consistently produces ~80% of a feature. The code works in isolation, but deployment configuration, environment variables, infrastructure updates, monitoring, and production readiness get skipped. The contract says "does the feature work?" but never asks "is it actually shipped?"

**The term "Stack Loops" and the specific pattern it describes — per-layer PICE loops across a technology stack with dependency ordering, always-run layers, and layer-specific contracts — are original to this framework.** The individual building blocks have conceptual precedent: Mike Cohn's Test Pyramid (2009) established layered testing, the "Dark Software Fabric" essay (2025) proposed a Speed Hierarchy of Feedback with 7 verification layers, and Spotify Engineering (2025) described verification loops for AI coding agents. Stack Loops synthesize these ideas into a novel formulation: iterative Plan→Implement→Contract-Evaluate cycles that run independently at each stack layer, where a feature is only complete when every layer passes.

> **Prior art analysis:** [Stack Loops and Arch Experts: Originality Analysis](research/originality-analysis.md) — systematic search across CrewAI, AutoGen, LangGraph, MetaGPT, Claude Code, academic papers, and developer communities confirming both terms are novel.

**The solution:** Instead of one PICE loop per feature, the system runs nested loops per stack layer. A feature is only PASS when every layer passes.

```
Feature: "Add user authentication"
│
├── PICE Loop: Backend Layer      → PASS ✅
├── PICE Loop: Database Layer     → PASS ✅
├── PICE Loop: API Layer          → PASS ✅
├── PICE Loop: Frontend Layer     → PASS ✅
├── PICE Loop: Infrastructure     → FAIL ❌ → fix → re-eval → PASS ✅
├── PICE Loop: Deployment         → PASS ✅
├── PICE Loop: Observability      → PASS ✅
│
└── Feature: ALL LAYERS PASS → SHIPPED ✅
```

### Key design principles

**Always-run layers.** Infrastructure, deployment, and observability layers never get skipped — those are where the missing 20% lives. A CSS-only change can skip the database layer. It cannot skip infrastructure.

**Layer-specific contracts.** Each layer has its own contract template with criteria that generic feature-level contracts miss. The infrastructure contract checks "are all env vars documented?", "does the Docker container build?", "are secrets managed properly?" The deployment contract checks "does it deploy to staging?", "does rollback work?", "is SSL configured?" These are the checks humans forget and AI assistants don't think to do.

**Dependency ordering.** Layers run in dependency order — backend before API, API before frontend, everything before deployment. A failure in an early layer blocks later layers.

**Configurable per project.** Default layers ship with the framework, but teams can add, remove, or reorder layers in `.pice/layers.toml`.

### Layer detection: how PICE discovers your stack

No existing tool defines "layers" the way Stack Loops needs them. Monorepo tools (Nx, Turborepo, Bazel) organize by *projects*. Package scanners (Snyk, Renovate) detect *manifests*. Framework detectors (Heroku buildpacks, Vercel) identify *runtimes*. None produce the architectural layer map Stack Loops requires. **PICE builds layer detection from scratch** using a heuristic combination of signals, with mandatory manual override for anything non-trivial.

**Detection strategy (layered heuristics):**

```
1. Manifest files     → Runtime, framework, dependencies
2. Directory patterns → app/, api/, infra/, deploy/, src/server/, src/client/
3. Framework signals  → Next.js app/ routes = frontend+API, Prisma = database
4. Config files       → Dockerfile, docker-compose, terraform/, .github/workflows/
5. Import graph       → Which files depend on which (static analysis)
6. Override file      → .pice/layers.toml (always wins)
```

**Fullstack-in-one frameworks** (Next.js, Remix, SvelteKit) are the hardest case. A single `pages/api/users.ts` that imports Prisma spans frontend, API, and database layers simultaneously. PICE handles this via **file-level layer tagging**: files can belong to multiple layers, and each layer's contract evaluates only the relevant aspects. The API layer contract evaluates route handling; the database layer contract evaluates query patterns; the frontend layer contract evaluates rendering. Same file, different evaluation lenses.

**Monorepos with multiple services** get treated as multiple stacks. Each service is a stack with its own layers. Shared libraries (auth utilities, TypeScript types) are tagged as **cross-stack dependencies** with their own seam checks — changes to shared code trigger re-evaluation in every consuming stack. This mirrors Nx's "affected" graph computation.

**Polyrepos** are the known limitation. PICE v0.2 operates within a single repository. Cross-repo seams (API contracts between separately deployed services) are deferred to v0.4's implicit contract inference, which uses distributed traces rather than source analysis. The `.pice/external-contracts.toml` file allows manually declaring external service contracts that seam checks verify against.

**The `.pice/layers.toml` format:**

```toml
[layers]
order = ["backend", "database", "api", "frontend", "infrastructure", "deployment", "observability"]

[layers.backend]
paths = ["src/server/**", "lib/**"]
always_run = false

[layers.infrastructure]
paths = ["terraform/**", "docker-compose.yml", "Dockerfile"]
always_run = true    # Never skipped regardless of change scope
type = "meta"        # Meta-layer: creates other layers, defines seams

[layers.deployment]
paths = [".github/workflows/**", "deploy/**", "runpod.toml"]
always_run = true
depends_on = ["infrastructure"]
environment_variants = ["staging", "production"]

[external_contracts]
api_gateway = { spec = "https://api.example.com/openapi.json", type = "openapi" }
```

Auto-detection generates a proposed `layers.toml` on `pice init`. The developer reviews, adjusts, and commits. Subsequent runs use the committed configuration, with PICE warning when new files don't match any layer pattern.

### Infrastructure-as-code: meta-layers, not peer layers

IaC (Terraform, Pulumi, CDK) is categorically different from application layers. It *creates* other layers, *defines* the seams between them, and *parameterizes* which contracts apply. The design models IaC as a **meta-layer** with distinct verification semantics:

- **Provisioning seams** (IaC → application) differ from **runtime seams** (API → database). Provisioning seams verify that infrastructure outputs match application inputs: "Does the provisioned database endpoint match DATABASE_URL?" Runtime seams verify operational behavior: "Does the API query pattern match the database schema?"
- **IaC verification is slow and expensive.** PICE uses tiered IaC checks: Tier 1 runs static analysis only (terraform validate, tfsec, checkov). Tier 2 adds AI evaluation of config correctness. Tier 3 adds plan-based verification (terraform plan → evaluate diff). Actual deployment testing is out of scope — that's what staging environments are for.
- **Multi-cloud deployments** get a two-dimensional model: layers × cloud providers. A single API layer deployed to both AWS and Azure has different IAM models, networking, and failure modes on each. Contracts at each intersection are evaluated independently.

### Environment-specific contracts

Contracts distinguish between **invariant properties** (true in all environments) and **environment-specific properties** (differ between dev, staging, production):

```toml
[contract.api]
# Invariant — always checked
response_format = "json"
auth_required = true

[contract.api.environments.production]
# Only checked when targeting production
ssl_required = true
rate_limiting = true
min_replicas = 2

[contract.api.environments.staging]
ssl_required = false
rate_limiting = false
```

Feature flags create **flag-state-indexed contracts**. Rather than testing all 2^N combinations (untenable), PICE uses pairwise coverage: each flag combination is tested with at least one other flag, covering interaction effects without combinatorial explosion. Contracts declare which flags affect which layers:

```toml
[feature_flags]
new_auth_flow = { affects_layers = ["api", "frontend"], default = false }
```

### Deployment transitions: canary, blue-green, rolling

During canary and blue-green deployments, multiple versions of a layer exist simultaneously. PICE models deployment transitions as **version-aware seams**:

- The database layer's contract must be **compatible with both old and new API versions** during the transition window (expand-and-contract migration pattern)
- Seam checks verify contract compatibility between the current production version and the incoming version
- `pice evaluate --transition` explicitly tests both versions against shared downstream contracts
- After full cutover, transition checks are retired

### Seam verification: the layer between the layers

Each Stack Loop iteration includes a **seam verification pass** — checking not just the component at a given layer, but its integration contracts with adjacent layers. This targets the twelve empirically validated failure categories from the seam blindspot research:

1. Configuration/deployment mismatches (Google: 31% of triggers; 82% from manual oversight)
2. Binary/version incompatibilities (Google: 37% of triggers)
3. Protocol/API contract violations (Adyen: 60K+ daily errors)
4. Authentication handoff failures (4.55% of all microservice issues)
5. Cascading failures from dependency chains (AWS 2025: 14-hour cascade)
6. Retry storm / timeout policy conflicts
7. Service discovery failures (>50% of practitioners in Gregor et al.)
8. Health check blind spots (AWS 2021: monitoring system itself failed to failover)
9. Serialization/schema drift between producer and consumer
10. Cold start and ordering dependencies between services
11. Network topology assumptions (Deutsch's Eight Fallacies, still validated)
12. Resource exhaustion at boundaries (thread pools, connection pools)

```
Feature: "Add user authentication"
│
├── PICE Loop: Backend Layer       → PASS ✅
│   └── Seam Check: Backend↔DB    → Does the ORM schema match the migration?
│   └── Seam Check: Backend↔API   → Do route handlers match the API contract?
│
├── PICE Loop: Database Layer      → PASS ✅
│   └── Seam Check: DB↔Infra      → Is the connection string in the env config?
│
├── PICE Loop: API Layer           → PASS ✅
│   └── Seam Check: API↔Frontend  → Do response types match frontend expectations?
│   └── Seam Check: API↔Auth      → Does the JWT flow work end-to-end?
│
├── PICE Loop: Infrastructure      → FAIL ❌
│   └── Seam Check: Infra↔Deploy  → JWT_SECRET missing from RunPod env ← CAUGHT
```

**The hardware VIP analogy.** In chip design, Synopsys and Cadence provide Verification IP (VIP) modules — protocol verification libraries that encode all protocol rules as executable assertions at every bus boundary. A single AMBA AXI interface has 44 verification rules. The verification logic travels with the interface definition, not with individual components. Software has no equivalent. PICE builds it: seam checks are protocol-specific verification modules that travel with layer boundary definitions.

### Adaptive evaluation: how many passes, and when to stop

A core question for Stack Loops: how many evaluation passes does each layer need? The answer is mathematically grounded, not arbitrary.

**The correlated evaluator ceiling.** The classical Condorcet Jury Theorem promises that majority-vote accuracy approaches 100% as evaluator count grows — but only when evaluators are independent. Kim et al. (ICML 2025) demonstrated across 350+ LLMs that models agree on ~60% of their errors, even across different providers. The effective sample size formula quantifies the damage: **n_eff = n / (1 + (n−1)ρ)**. With ρ ≈ 0.3 between Claude and GPT, the effective independent evaluator count caps at ~3.3 regardless of pass count. The practical ceiling is **~97% accuracy, reached by approximately 5 passes**.

| Passes | Effective N | Estimated confidence | Marginal gain |
| ------ | ----------- | -------------------- | ------------- |
| 1      | 1.0         | 88.0%                | —             |
| 2      | 1.5         | 92.1%                | +4.1%         |
| 3      | 1.9         | 94.0%                | +1.9%         |
| 5      | 2.3         | 95.4%                | +0.7% avg     |
| 10     | 2.6         | 96.2%                | +0.16% avg    |
| ∞      | 2.9         | ~96.6%               | 0             |

*Assumptions: individual evaluator accuracy p = 0.88, inter-model correlation ρ = 0.35. Based on correlated Condorcet Jury Theorem and Chernoff bound analysis.*

**Passes 1→3 capture 75% of total achievable improvement. Passes 1→5 capture 92%.** This mathematically justifies PICE's tiered architecture and means the framework's strategic advantage lies not in running more passes but in *adaptively allocating* passes based on accumulated evidence.

**Breaching the ceiling requires architectural diversity, not more passes.** Three strategies push beyond ~97%: (1) maximize evaluator diversity — architecturally distinct models reduce ρ, (2) incorporate orthogonal verification signals — unit tests, static analysis, and formal verification are essentially uncorrelated with LLM judgment errors, and (3) decompose evaluation into independent sub-problems, each with its own evaluator committee.

> **Expanded research:** [Optimal Verification Passes in Multi-Model AI Evaluation](research/convergence-analysis.md) — full mathematical derivations, Chernoff bound analysis, empirical data from SWE-Bench/AlphaCode/self-consistency studies, confidence curve modeling.

### Three novel algorithms for adaptive verification

PICE introduces three algorithms for dynamically determining evaluation depth. These algorithms draw on mature mathematical foundations from sequential analysis, information theory, and psychometrics, but **their combination and application to multi-model code verification is unpublished**.

**Algorithm 1: Bayesian-SPRT Adaptive Halting.** Fuses Bayesian belief updating with Wald's Sequential Probability Ratio Test. Each evaluation pass updates a Beta posterior on P(code_correct), while a log-likelihood ratio is compared against SPRT acceptance/rejection thresholds. The Wald-Wolfowitz theorem guarantees optimal expected sample size among all tests with equivalent error control. Expected passes: **E[N] ≈ log(1/δ) / D_KL(p₁ ∥ p₀)** — approximately 3.2 passes for an evaluator with 85% accuracy at α=0.05, β=0.10. An O'Brien-Fleming alpha-spending overlay uses stringent early thresholds, preserving discriminative power for later passes.

*Foundations: Wald (1947) Sequential Analysis, O'Brien & Fleming (1979) group sequential designs, Bayesian posterior-based stopping rules (Eckman & Henderson, 2020). Prior application to LLM: ConSol (Lee et al., March 2025) applied SPRT to single-model self-consistency, but never to heterogeneous multi-model code evaluation with Bayesian priors.*

**Algorithm 2: Adversarial Divergence-Triggered Scaling (ADTS).** Uses inter-model disagreement as the control signal for evaluation depth, drawing on Query by Committee from active learning and Knowledge Divergence theory. The scaling rule: if disagreement D_n < τ_low after 2 passes (strong agreement), halt. If D_n > τ_high (strong disagreement), escalate to a third model as tiebreaker and apply Bayesian-SPRT to the expanded committee. This creates a natural three-tier flow:

- **Tier 1** (~70% of evaluations): 2 passes, both models agree → accept/reject
- **Tier 2** (~25%): 3–5 passes, moderate disagreement → targeted additional passes
- **Tier 3** (~5%): 5+ passes or human escalation → deep evaluation or formal verification

*Foundations: Seung et al. (1992) Query by Committee, Kaplan et al. (2025) Knowledge Divergence theory proving debate advantage scales with representational diversity, Du et al. (2024) showing mixed-model debates plateau after ~4 rounds.*

**Algorithm 3: Verification Entropy Convergence (VEC).** Adapts semantic entropy (Kuhn et al., ICLR 2023) and Predicted Standard Error Reduction from psychometric adaptive testing to create a stopping rule based on information content. Halt when (a) semantic entropy over evaluator outputs drops below threshold ε, AND (b) marginal entropy reduction |SE_n − SE_{n−1}| < δ. A critical innovation: decompose verification entropy into epistemic (models don't understand) and aleatoric (spec is genuinely ambiguous) components. High epistemic → more passes. High aleatoric → escalate to human, since more model passes cannot resolve inherent ambiguity.

*Foundations: Shannon (1948) information theory, Kuhn et al. (2023) semantic entropy for LLM uncertainty, Choi et al. (2010) Predicted Standard Error Reduction in computerized adaptive testing, PROMIS CAT stopping rules (SE < 0.3, 4–12 items).*

### Claude Code integration: subagent-per-layer isolation

Each Stack Loop layer runs as a Claude Code **subagent** via the stable `Agent` tool:

- **Context isolation.** Each subagent gets a fresh, isolated context window. The evaluator at layer N cannot be contaminated by layer N-1 reasoning. Only the final result returns to the parent.
- **Read-only evaluation agents.** The `tools` field restricts evaluators to `[Read, Grep, Glob]` — no `Write`, `Edit`, or `Bash`.
- **Cost-capped evaluation.** `maxTurns` caps agent loop iterations per layer.
- **Model tiering per layer.** Each subagent accepts `model` parameter (`haiku`/`sonnet`/`opus`/full ID).

**Architectural constraint:** Subagents cannot spawn subagents (no recursive nesting). PICE's Rust coordinator is the sole parent, sequentially spawning each layer's evaluator — which is exactly the flat orchestration model Stack Loops require.

**Adversarial evaluation remains at the PICE layer.** The Claude Agent SDK only supports Anthropic models. Dual-model adversarial evaluation (Claude + GPT) is orchestrated by PICE's Rust core: Claude-side via the Agent SDK, GPT-side via separate OpenAI API, PICE merging results. The ADTS algorithm determines whether additional passes are needed based on inter-model divergence.

> **Expanded research:** [Claude Code Agent Teams: Technical Deep-Dive for PICE](research/claude-code-integration.md) — subagent vs. Agent Teams analysis, Agent tool schema, SDK integration options, CLI subprocess architecture, licensing analysis.

### Incremental re-evaluation: the bidirectional dependency graph

When a developer fixes a failing layer, what happens to downstream layers? What about upstream layers that already passed — could a fix invalidate an earlier pass?

Standard build systems (Nx, Turborepo, Bazel) assume unidirectional dependency flow: changes propagate downstream only. **Stack Loops require bidirectional awareness** because a fix in the infrastructure layer might change a contract that the API layer was already verified against.

**The model: contract-based change pruning** (adapted from Bazel's change pruning):

```
Layer fixed → Re-verify that layer → Compare output contracts with prior version
│
├── Contracts unchanged → No downstream re-verification needed
│
├── Contracts changed (downstream) → Re-verify downstream consumers
│
└── Contracts changed (upstream) → Re-verify upstream layers that
    consumed the changed contract (backward edge propagation)
```

The verification manifest (see Crash Recovery below) tracks which contract version each layer was verified against. After a fix, PICE compares the new contract hash against the stored hash. If they match — the fix changed internals but not the interface — downstream and upstream layers keep their PASS status. If the contract changed, only layers consuming that specific contract are re-verified.

This is significantly more efficient than re-running everything. In practice, most fixes don't change contracts — they fix implementation bugs that don't alter the interface. The contract-based pruning skips re-verification in the majority of cases.

### Crash recovery: verification manifests

If PICE verifies 10 layers and crashes on layer 7, it must not re-verify layers 1–6. The system maintains a **verification manifest** — a persistent record of completed verification:

```json
{
  "feature": "add-user-auth",
  "plan_hash": "sha256:abc123...",
  "layers": {
    "backend": {
      "status": "pass",
      "confidence": 0.941,
      "passes": 3,
      "cost_usd": 0.031,
      "contract_hash": "sha256:def456...",
      "model_versions": { "claude": "sonnet-4-20250514", "gpt": "gpt-4o-2025-03" },
      "timestamp": "2026-04-05T14:23:01Z"
    },
    "database": { "status": "pass", "..." : "..." },
    "api": { "status": "running", "pass_number": 2 }
  },
  "seams": {
    "backend↔database": { "status": "pass", "..." : "..." }
  }
}
```

On resume, PICE reads the manifest, skips completed layers (whose content hashes haven't changed), and continues from the last incomplete layer. Each layer verification is **idempotent**: same code + same model version + same prompt = same result, within stochastic variance.

The manifest also enables `pice status` to show progress even after a crash — which layers passed, which were in progress, and which are pending.

### Resilience: provider outages and model drift

**Dual-provider outage fallback.** If Claude Code or OpenAI is unavailable, PICE degrades gracefully through four tiers:

```
Tier A: Full AI verification (Claude + GPT adversarial)     ← normal operation
Tier B: Single-model verification (whichever provider is up) ← one provider down
Tier C: Cached results for unchanged layers + static checks  ← both providers down
Tier D: Skip AI verification with prominent warning          ← emergency bypass
```

The system never silently proceeds without verification. Each degradation tier is logged, and `pice status` shows the degradation level. Teams can configure minimum acceptable degradation in `.pice/config.toml`:

```toml
[resilience]
min_verification_tier = "B"    # Block if both providers are down
fallback_timeout_seconds = 30  # Time before degrading to next tier
```

**Model version pinning.** AI model updates can silently change evaluation behavior — Apple's MUSCLE research found that model updates cause "negative flips" where previously correct evaluations become incorrect. PICE addresses this with:

- **Pinned model versions** in `.pice/config.toml`: `claude_model = "claude-sonnet-4-20250514"` rather than `"sonnet"`
- **Evaluation regression tests**: a `.pice/golden-evaluations/` directory containing known inputs and expected outputs. On model version change, PICE runs the golden suite and warns if results diverge beyond a configurable threshold
- **Consensus voting**: for critical Tier 3 checks, run evaluation against both the old and new model version. If they disagree, flag for human review before accepting the new version

### CI/CD integration: staying under the 10-minute wall

Research shows developers context-switch away from CI after 6–7 minutes (Honeycomb Engineering). Each additional 5 minutes of CI time increases average time-to-merge by over an hour (Graphite). **Sequential verification of all layers is a dealbreaker** — 10 layers at 2–4 minutes each = 20–40 minutes.

**Four strategies keep total time under 10 minutes:**

**1. Path-based filtering (biggest impact).** Only verify layers whose files changed. A CSS-only change skips backend, database, API, and infrastructure layers — verifying only frontend + always-run layers (deployment, observability). Implemented via `pice affected` which computes the changed layer set from the git diff.

**2. Parallel layer execution.** Independent layers (backend and frontend have no dependency edge) run their PICE loops concurrently. The dependency graph in `.pice/layers.toml` determines which layers can parallelize. Claude Code subagents support `run_in_background: true` for concurrent execution.

**3. Tiered model routing.** Haiku (~100ms response) for simple checks. Sonnet (~2s) for standard evaluation. Opus (~5s) only for complex Tier 3 analysis. Most Tier 1 evaluations complete in under 30 seconds per layer.

**4. Prompt caching.** Anthropic's prompt caching reduces costs by 90% and latency by 85% on repeated context. Layer contracts and system prompts are cached across runs. Only the changed code is new context.

**CI integration pattern (GitHub Actions):**

```yaml
# .github/workflows/pice.yml
name: PICE Verification
on: [pull_request]

jobs:
  pice-evaluate:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: pice-framework/action@v0.2
        with:
          tier: 2                    # Tier level for this pipeline
          budget_usd: 2.00           # Cost circuit breaker
          timeout_minutes: 10        # Time circuit breaker
          min_confidence: 0.90       # Minimum acceptable confidence
        env:
          ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
          OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}
```

**Cost circuit breakers are non-negotiable.** A 500-file monorepo can consume 500K+ tokens per API call. At Sonnet pricing ($3/M input), a 10-layer verification could cost $1.50–$4.50 per run. PICE enforces hard budget limits per evaluation, per PR, and per billing period. The Anthropic Batch API (50% cost reduction) is used for non-interactive CI runs.

### Cross-layer contract format

Every existing contract format covers exactly one layer: OpenAPI for REST, AsyncAPI for events, Protobuf for gRPC, Prisma for databases. **No format spans the full stack.** PICE defines a unified contract format in YAML with JSON Schema validation:

```yaml
# .pice/contracts/api-layer.contract.yaml
schema_version: "0.2"
layer: api
depends_on: [backend, database]

checks:
  structural:
    - id: route-handler-match
      description: "All route handlers have corresponding backend implementations"
      severity: critical
      
  seams:
    - id: api-frontend-response-types
      boundary: api↔frontend
      description: "Response types match frontend TypeScript interfaces"
      failure_category: serialization_drift    # From the 12-category taxonomy
      severity: high
      
    - id: api-auth-jwt-flow
      boundary: api↔auth
      description: "JWT flow works end-to-end across auth boundary"
      failure_category: auth_handoff
      severity: critical
      
  environment:
    production:
      - id: ssl-termination
        description: "SSL is properly terminated"
      - id: rate-limiting
        description: "Rate limiting is configured"

metadata:
  auto_generated: true
  last_detection: "2026-04-05"
  contract_hash: "sha256:abc123..."
```

The `failure_category` field links each check to the twelve empirically validated failure categories, ensuring coverage across all known seam failure modes. Auto-generated contracts can be manually refined and committed — the committed version always takes precedence over auto-detection.

### Onboarding: five minutes or less on existing projects

Survey data: 34.2% of developers abandon a tool if setup is painful — the #1 abandonment trigger. PICE targets under 5 minutes from install to first evaluation:

```bash
# Install
cargo install pice          # or: npm install -g pice

# Initialize on existing project (auto-detects layers, generates config)
cd my-project
pice init
# ✅ Detected: Next.js 15, PostgreSQL, Prisma, Docker, GitHub Actions
# ✅ Generated: .pice/layers.toml (7 layers, review and commit)
# ✅ Generated: .pice/contracts/ (5 auto-generated contracts)
# ✅ Ready: run `pice evaluate` to verify current state

# First evaluation runs in "baseline mode" — reports only, never blocks
pice evaluate --baseline
# ╔═══════════════════════════════════════════════╗
# ║  Baseline Scan (informational, non-blocking)  ║
# ╠═══════════════════════════════════════════════╣
# ║  ✅ Backend       PASS  (2 passes)            ║
# ║  ⚠️ Database      3 findings (non-blocking)   ║
# ║  ✅ API           PASS  (2 passes)            ║
# ║  ⚠️ Infrastructure 5 findings (non-blocking)  ║
# ╠═══════════════════════════════════════════════╣
# ║  8 findings for review — see .pice/baseline/  ║
# ║  Run `pice evaluate` (no --baseline) to       ║
# ║  enforce verification on future changes.      ║
# ╚═══════════════════════════════════════════════╝
```

**Baseline mode** follows the TypeScript adoption playbook: start permissive, tighten gradually. The initial scan establishes the current state without blocking. Findings go into `.pice/baseline/` for review. The team gradually addresses baseline findings and enables enforcement layer by layer. A flood of initial errors on an existing project is a guaranteed adoption killer — baseline mode prevents this.

**Every failed check includes actionable diagnostics:**
- What layer and check failed
- The specific contract criterion violated
- The code location (file, line, function)
- A suggested fix (AI-generated)
- The confidence level of the finding
- Whether this is a seam check (and which boundary pair)

Low-confidence findings are marked as such — presenting uncertain findings as definitive erodes trust faster than not flagging them at all.

> **Expanded research:** [Stack Loops v0.2 Gap Analysis](research/v02-gap-analysis.md) — full 37-gap analysis across 8 dimensions, competitive landscape (Qodo, SonarSource AC/DC, IronBee, Opslane), incremental re-evaluation models (Bazel, Adapton, pluto), CI timing research, feature flag combinatorics.

### Planned interface

```bash
# Plan generates sections for each relevant layer
pice plan "add user auth"

# Execute and evaluate run per layer
pice execute .claude/plans/auth-plan.md
pice evaluate .claude/plans/auth-plan.md

# Target a specific layer
pice execute .claude/plans/auth-plan.md --layer infrastructure
pice evaluate .claude/plans/auth-plan.md --layer deployment

# Seam verification across all layer boundaries
pice seams .claude/plans/auth-plan.md

# Status shows layer-level progress including seam checks
pice status
# ╔═══════════════════════════════════════════╗
# ║  Feature: Add user authentication         ║
# ╠═══════════════════════════════════════════╣
# ║  ✅ Backend       PASS  (2 passes, 92.1%) ║
# ║  ✅  └─ seams     2/2 verified            ║
# ║  ✅ Database      PASS  (2 passes, 93.4%) ║
# ║  ✅  └─ seams     1/1 verified            ║
# ║  ✅ API           PASS  (4 passes, 95.1%) ║
# ║  ⚠️  └─ seams     1/2 — JWT handoff warn  ║
# ║  ✅ Frontend      PASS  (2 passes, 91.8%) ║
# ║  ⏳ Infra         RUNNING... pass 3/5     ║
# ║  ⬜ Deployment    PENDING                  ║
# ║  ⬜ Observability PENDING                  ║
# ╠═══════════════════════════════════════════╣
# ║  Feature: 4/7 layers  •  4/6 seams        ║
# ║  Confidence: 94.2% (target: 95%)          ║
# ║  Cost: $0.47 / budget: $2.00              ║
# ╚═══════════════════════════════════════════╝
```

### Tier integration

Layer count, seam depth, and evaluation passes scale with tier:

| Tier   | Layer scope                  | Seam verification                              | Evaluation passes              | Evaluation method              |
| ------ | ---------------------------- | ---------------------------------------------- | ------------------------------ | ------------------------------ |
| Tier 1 | Affected layers only         | Adjacent seams only                            | 2 (ADTS fast path)            | Single evaluator per layer     |
| Tier 2 | Affected layers + always-run | Adjacent + transitive seams                    | 2–5 (Bayesian-SPRT adaptive)  | Dual-model per layer           |
| Tier 3 | All layers                   | Full seam graph + adversarial assumption mining | 3–10 (VEC convergence-driven) | Agent team + adversarial + formal |

-----

## v0.3 — Arch Experts *(novel concept, coined in PICE)*

**The problem:** No single AI model has deep expertise across every technology in a modern stack. A model that writes excellent Next.js code may not know that RunPod serverless handlers need explicit concurrency settings, or that Docker Hub team repos require specific push authentication, or that your CI pipeline needs new secrets added when you introduce JWT.

Pre-built agent libraries (CrewAI, AutoGen) require you to manually select and configure specialists for every technology combination. This doesn't scale — the number of possible stack combinations is effectively infinite.

**The term "Arch Experts" and the specific pattern it describes — dynamically generated specialist agents inferred from project architecture files — are original to this framework.** Related ideas exist under different names: AutoAgents (Chen et al., 2023) dynamically synthesizes expert agents from task content, the Codified Context paper (Vasilopoulos, 2026) describes 19 manually authored domain-expert agents with trigger routing, CMU's ArchE (2003–2008) was a rule-based architecture design assistant, and MetaGPT (2023) includes a fixed Architect role. Arch Experts are distinct in that they are neither pre-built nor manually configured — they emerge automatically from your project's actual configuration files, generating technology-specific system prompts and contract criteria without a template library.

**The solution:** PICE reads your project's architecture files and dynamically generates specialized expert agents for your specific stack. No library needed. The experts emerge from your actual configuration.

### How it works

**Architecture Discovery.** On `pice plan` or `pice prime`, the system scans your project for technology signals:

```
Discovered architecture:
├── Runtime: Node.js 22 (package.json)
├── Framework: Next.js 15 (dependencies)
├── Database: PostgreSQL 16 (docker-compose.yml)
├── ORM: Drizzle (dependencies)
├── Deployment: Docker → RunPod Serverless (Dockerfile + runpod.toml)
├── Registry: Docker Hub, team repo "revgen" (docker-compose.yml)
├── CI/CD: GitHub Actions (.github/workflows/)
├── Monitoring: None detected ⚠️
└── Env management: .env + Docker secrets
```

**Expert Spawning via Runtime AgentDefinitions.** For each detected technology, PICE constructs an `AgentDefinition` object at runtime — not from a template library, but dynamically from your project's actual configuration. These are passed inline via the Agent SDK's `agents` parameter or `--agents <json>` CLI flag, avoiding filesystem mutation.

```typescript
const agents = {
  "runpod-expert": {
    description: "RunPod Serverless deployment specialist",
    prompt: `You are a RunPod Serverless expert reviewing deployment readiness.
Project context: handler.py uses JWT auth middleware.
Verify: JWT_SECRET in RunPod env config, cold start impact,
handler timeout settings for auth-heavy requests...`,
    tools: ["Read", "Grep", "Glob"],
    model: "sonnet"
  }
};
```

### Seam Experts: owning the boundaries, not just the components

In v0.3, Arch Experts evolve beyond component-level expertise to **own the seams around their components**. Each expert is responsible for:

1. **What this component provides** — the behavioral contracts downstream consumers can rely on
2. **What this component assumes** — the behavioral contracts it depends on from upstream providers

```
Spawned experts:
├── Next.js 15 Expert
│   ├── Provides: SSR responses, API route contracts, static asset paths
│   └── Assumes: API responds within 200ms, auth tokens in specific header format
│
├── PostgreSQL + Drizzle Expert
│   ├── Provides: Schema guarantees, migration ordering, connection pooling
│   └── Assumes: DATABASE_URL set, max connections configured, SSL in production
│
├── RunPod Serverless Expert
│   ├── Provides: Handler response format, scaling behavior, cold start bounds
│   └── Assumes: All env vars present, model weights accessible, timeout > handler duration
│
└── Docker + Docker Hub Expert
    ├── Provides: Image builds, layer caching, registry availability
    └── Assumes: Build args present, registry auth configured, base image accessible
```

**Adversarial assumption mining.** At Tier 3, the dual-model adversarial evaluation is repurposed for seam discovery. One model (Claude) infers what the consumer assumes. The other (GPT) independently infers what the provider guarantees. PICE compares the two sets. Any assumption in the consumer's list that's absent from the provider's guarantees is a **seam gap** — a latent integration failure waiting to happen.

This is the capability that doesn't exist anywhere: **automated cross-component assumption asymmetry detection**. Garlan et al. identified it as the core problem in 1995. Thirty years later, PICE builds the automated detection.

The ADTS algorithm (from v0.2) governs how deep the adversarial mining goes: if Claude and GPT's assumption lists diverge significantly (high D_n), the system escalates to additional passes with targeted prompts probing the specific seams where divergence is highest.

**Expert-Augmented Contracts with Seam Criteria:**

```
Infrastructure contract criteria:
├── [base]                      All env vars documented in .env.example
├── [docker-expert]             Image builds with new auth dependencies
├── [docker-expert]             Image pushed to revgen team repo
├── [runpod-expert]             Handler initializes auth middleware correctly
├── [runpod-expert]             JWT_SECRET configured in RunPod env
├── [runpod-expert]             Cold start under 5s with auth middleware
├── [actions-expert]            New secrets added to deployment workflow
├── [seam: api↔runpod]         Response timeout > handler execution time
├── [seam: docker↔registry]    Push auth matches team repo permissions
└── [seam: runpod↔env]         All handler env vars present in deployment config
```

### Expert team sizing

Community experience with Claude Code agent teams consistently shows diminishing returns beyond 3 teammates, with the official recommendation of 5–6 tasks per teammate. This is also supported by the ensemble theory underpinning the convergence analysis: the Krogh-Vedelsby decomposition shows **E_ensemble = E_avg − Ambiguity** — ensemble error improves only to the extent evaluators bring genuinely different perspectives. 2–3 specialists per review maximizes ambiguity (evaluator diversity) without hitting the correlated ceiling.

### The strategic pipeline use case

Stack Loops and Arch Experts aren't just for features you already know how to build. They're for when you have a strategic idea — a complex, multi-service pipeline — but aren't sure how to connect all the dots.

Example: a multi-model inference pipeline where requests hit an API gateway, route to either a quantized model (fast ETL) or full-weight model (deep analysis), with results stored in a vector database, served through RunPod, and monitored via structured logging.

With Stack Loops, every connection point has its own layer contract plus seam checks. With Arch Experts, each technology gets a dynamically spawned specialist that knows the integration gotchas and owns the seams. The system doesn't just help build the parts — it ensures the seams between parts actually hold. The ADTS algorithm concentrates evaluation budget on the boundaries where expert disagreement is highest — the exact places where integration failures lurk.

### Architecture-inferred, not configured

You never manually specify which experts to use:

| File                                         | Infers                                      |
| -------------------------------------------- | ------------------------------------------- |
| `package.json`                               | Runtime, framework, dependencies            |
| `Dockerfile`                                 | Container config, base image, build steps   |
| `docker-compose.yml`                         | Services, databases, networking, registries |
| `runpod.toml` / `fly.toml` / `vercel.json`  | Deployment target and config                |
| `.github/workflows/*.yml`                    | CI/CD pipeline structure                    |
| `drizzle.config.ts` / `prisma/schema.prisma` | ORM and database schema                     |
| `.env.example`                               | Environment variable requirements           |
| `tsconfig.json` / `pyproject.toml`           | Language and tooling config                 |

Add a new technology to your stack → the next `pice plan` automatically spawns the right expert and discovers the new seams.

-----

## v0.4 — Implicit Contract Inference *(novel capability, no prior art)*

**The problem:** The most dangerous integration failures come from assumptions that are never written down. "This endpoint always returns within 200ms." "This queue never has more than 1000 items." "This service starts before that service." These are implicit contracts — behavioral properties the system depends on but no one has documented, no schema enforces, and no test verifies.

**This capability does not exist in any current tool, framework, or research prototype.** The closest work: Daikon (U. of Washington) infers invariants within a single component. Spec mining (Ammons et al.) infers protocols from execution traces. Signadot SmartTests infer API contracts from observed traffic. But no tool applies cross-service behavioral inference at integration boundaries, establishes baselines, monitors for drift, or evaluates proposed changes against discovered contracts.

### Five capabilities that close the gap

**1. Cross-component assumption asymmetry detection.** PICE independently infers what each side of an integration boundary assumes and guarantees. Consumer A's code implies "this field is always present." Provider B's schema says "this field is optional." That's a seam gap. No tool does this today.

**2. Implicit contract inference from traffic.** PICE analyzes distributed traces at service boundaries and infers behavioral contracts: "responses always arrive within 150ms at p95," "this endpoint is always called after that one," "this field has never been null despite being schema-optional."

**3. Seam drift detection.** PICE establishes a behavioral baseline at each integration point and continuously monitors for gradual divergence — response time distributions shifting, optional fields becoming always-present, ordering guarantees weakening. This is SLO monitoring for discovered (not declared) behavioral properties.

**4. Change impact analysis against implicit contracts.** Before deployment, PICE evaluates proposed changes against inferred implicit contracts. "This change moves p95 latency from 180ms to 250ms, and Service B assumes responses within 200ms."

**5. Adversarial integration test generation.** PICE mines implicit assumptions from observed behavior and generates targeted tests probing those specific assumptions. Not random fuzzing — targeted assumption validation.

### The synthesis no one has attempted

| Research lineage                        | Contribution to PICE                               | Maturity        |
| --------------------------------------- | -------------------------------------------------- | --------------- |
| Daikon (invariant detection)            | Infer behavioral properties from execution         | Mature (20+ yr) |
| Spec mining (protocol inference)        | Infer ordering/sequencing from traces              | Academic        |
| OpenTelemetry (distributed tracing)     | Observation infrastructure at service boundaries   | Widely adopted  |
| Session types (behavioral protocols)    | Formal framework for protocol correctness          | Theoretical     |
| Hardware VIP (interface verification)   | Verification logic embedded in interface defs      | Industry standard (HW) |
| Chaos engineering (assumption testing)  | Validate assumptions through controlled failure    | Adopted (Netflix) |
| **PICE: the synthesis**                 | All of the above, automated, at every seam         | **Novel**       |

> **Expanded research:** [The Seam Blindspot](research/seam-blindspot.md) — full cross-domain analysis including session type implementations in Rust (mpst-rust), TLA+ usage at Amazon, RESTler stateful API fuzzing, DO-178C bidirectional traceability, and AUTOSAR interface verification.

-----

## v0.5 — Self-Evolving Verification *(novel closed-loop architecture)*

**The problem:** Static verification frameworks produce the same results whether they've run once or a thousand times. The checks don't learn. The thresholds don't adapt. The cost doesn't optimize. Every execution starts from zero.

PICE v0.1's SQLite metrics engine was the seed. v0.5 grows it into a closed-loop system where **every execution makes the next execution smarter, more targeted, and cheaper**. The framework compounds in value over time.

### The MAPE-K control loop

PICE's self-evolution follows IBM's MAPE-K architecture (2001), the canonical reference for self-adaptive systems, adapted for AI-assisted verification:

```
┌─────────────────────────────────────────────────┐
│                  KNOWLEDGE                       │
│            (SQLite metrics engine)                │
│                                                   │
│  Per-check hit rates    │  Model reliability      │
│  False positive rates   │  Cost per true positive  │
│  Layer failure patterns │  Seam check frequency    │
│  Expert accuracy scores │  Convergence curves      │
└──────────┬──────────────┴────────────┬───────────┘
           │                           │
    ┌──────▼──────┐             ┌──────▼──────┐
    │   MONITOR   │             │   EXECUTE   │
    │             │             │             │
    │ Collect per │             │ Apply config│
    │ evaluation: │             │ changes:    │
    │ • verdict   │             │ • thresholds│
    │ • confidence│             │ • model tier│
    │ • tokens    │             │ • prompts   │
    │ • latency   │             │ • budgets   │
    │ • model used│             │             │
    └──────┬──────┘             └──────▲──────┘
           │                           │
    ┌──────▼──────┐             ┌──────┴──────┐
    │   ANALYZE   │────────────▶│    PLAN     │
    │             │             │             │
    │ Compute:    │             │ Generate:   │
    │ • rolling   │             │ • check     │
    │   averages  │             │   enable/   │
    │ • trends    │             │   disable   │
    │ • anomalies │             │ • model     │
    │ • Bayesian  │             │   routing   │
    │   estimates │             │ • prompt    │
    │             │             │   candidates│
    └─────────────┘             └─────────────┘
```

*Foundations: IBM MAPE-K (Kephart & Chess, 2003). Recent critiques (FSE 2025: "Breaking the Loop: AWARE is the New MAPE-K") suggest enhancing with proactive, LLM-driven Analyze and Plan phases — which PICE's AI evaluators already provide.*

### Double-loop learning: tuning vs. evolving

**Single-loop learning** (the inner loop) adjusts actions within existing rules: change thresholds, reassign models, adjust budget allocation. Like a thermostat maintaining temperature.

**Double-loop learning** (the outer loop) questions the rules themselves: are the verification criteria correct? Are new checks needed? Does the architectural model still reflect reality? Like questioning whether 68°F is the right target.

*Foundation: Argyris & Schön (1978) double-loop learning. Applied to adaptive management (PMC: Rizzari et al., 2018) but never to AI verification frameworks.*

For PICE, the inner loop continuously tunes evaluation parameters. The outer loop, triggered by sustained metric degradation or pattern analysis, generates new checks, retires obsolete ones, or restructures the seam model. This is where the framework genuinely evolves rather than merely adjusting.

### The seven core metrics (minimum viable telemetry)

Based on DORA metrics research, control theory, and the software entropy framework:

1. **Per-check hit rate** — rolling 30-day window. Which checks actually catch issues?
2. **Per-check false positive rate** — ground truth from manual review or production correlation.
3. **Per-layer failure distribution** — which architectural seams are most frequently violated?
4. **Cost per evaluation** — tokens and dollars, per model.
5. **Evaluation latency** — p50, p95, p99.
6. **Model agreement rate** — Cohen's kappa for inter-rater reliability in dual-model checks.
7. **Defect escape rate** — production issues not caught by verification.

From these, the system computes a **check value score**: `(hit_rate × severity_weight × (1 − FPR)) / cost_per_run`. This single metric enables direct comparison of check ROI and drives all automated optimization decisions.

### What the system does with the data

**Phase 1 — Predictive check selection.** Meta's PTS system catches >99.9% of faulty code changes while running only one-third of tests. Applied to PICE: train a model on historical check outcomes, predict which checks are most likely to catch issues for a given code change, prioritize accordingly. The feature set — check outcomes, file associations, layer information — is exactly what PICE's SQLite engine already collects.

**Phase 2 — DSPy-style prompt optimization.** Rather than manually tuning Arch Expert prompts, define metric functions (accuracy, precision, cost) and let optimizers (MIPROv2, BootstrapFewShot) systematically search the instruction space using accumulated evaluation traces. Reported gains: GPT-4o-mini scores from 66% to 87% on classification tasks through automated prompt optimization.

**Phase 3 — Autonomous check evolution.** Analyze patterns in historical failures — which boundaries fail most, what code patterns trigger violations, what new violation types emerge — and generate candidate verification checks. Candidates enter a probation period where hit rate, FPR, and value score are tracked. Checks that prove value get promoted; those that don't get pruned.

*Foundations: Meta Predictive Test Selection (ICSE-SEIP 2019, >99.9% recall at 33% test execution). Develocity/Netflix (280K developer hours saved/year). DSPy (Stanford NLP, MIPROv2 optimizer). SICA (ICLR 2025, 17–53% self-improvement on SWE-Bench). DeepVerifier (2025, 12–48% improvement via self-evolving rubrics).*

### Evaluation-to-production correlation: the ultimate ground truth

The single most important metric for self-evolution: **do PICE's verification verdicts predict production incidents?** This is where Observability-Driven Development (Charity Majors / Honeycomb) meets verification: track whether code that passed all checks actually works in production, and whether code flagged by checks actually would have caused incidents.

Over time, this correlation score becomes the tuning signal for the entire system. Checks that predict production issues get amplified. Checks that don't get deprioritized. The framework learns what actually matters — not what looked important in theory.

> **Expanded research:** [Self-Evolving Verification Frameworks: State of the Art](research/self-evolving-verification.md) — Meta/Google/Develocity predictive test selection systems, Reflexion/DSPy/SICA self-improving agent architectures, MAPE-K implementation patterns, minimum viable telemetry schema, evolutionary test optimization.

-----

## Architecture Diagrams

### System Architecture: The Full PICE Flow

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           THE SEAM PROBLEM                                  │
│  "Software breaks at boundaries, not inside components"                     │
│  68% of outages from integration points (Google SRE)                        │
│  AI agents fail 75%+ on cross-component changes (SWE-Bench Pro)            │
└─────────────────────────────┬───────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                        PICE CORE (Rust)                                     │
│                   Sole orchestrator & decision engine                        │
│                                                                             │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐   │
│  │  PLAN        │→ │  IMPLEMENT   │→ │  CONTRACT    │→ │  EVALUATE    │   │
│  │              │  │              │  │              │  │              │   │
│  │ Architecture │  │ Code gen via │  │ Layer-spec + │  │ Dual-model   │   │
│  │ discovery    │  │ Claude Code  │  │ seam criteria│  │ adversarial  │   │
│  │ Expert spawn │  │ subagents    │  │ from experts │  │ + algorithms │   │
│  └──────────────┘  └──────────────┘  └──────────────┘  └──────┬───────┘   │
│                                                                │           │
│                              ┌──────────────────────────────────┘           │
│                              ▼                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                    ADAPTIVE EVALUATION ENGINE                       │   │
│  │                                                                     │   │
│  │  Pass 1 → Claude evaluates → Update Beta posterior                  │   │
│  │  Pass 2 → GPT evaluates   → Check SPRT boundaries                  │   │
│  │                                                                     │   │
│  │  ┌─ ADTS Decision ─────────────────────────────────────────────┐   │   │
│  │  │                                                              │   │   │
│  │  │  Agreement (D < τ_low)?  ──→  HALT (Tier 1, ~70%)          │   │   │
│  │  │  Moderate disagreement?  ──→  Pass 3–5 targeted (Tier 2)   │   │   │
│  │  │  Strong disagreement?    ──→  Escalate + VEC (Tier 3)      │   │   │
│  │  │                                                              │   │   │
│  │  └──────────────────────────────────────────────────────────────┘   │   │
│  │                                                                     │   │
│  │  VEC: Stop when semantic entropy converges                          │   │
│  │  Bayesian-SPRT: Stop when posterior crosses threshold                │   │
│  │  Confidence output: 88% → 92% → 94% → 95.4% (diminishing returns) │   │
│  └─────────────────────────────────────────────────┬───────────────────┘   │
│                                                     │                      │
└─────────────────────────────────────────────────────┼──────────────────────┘
                                                      │
              ┌───────────────────────────────────────┘
              ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                         STACK LOOPS (v0.2)                                   │
│                   Per-layer verification with seam checks                    │
│                                                                             │
│  ┌──────────┐  ┌─────┐  ┌──────────┐  ┌─────┐  ┌──────────┐              │
│  │ Backend  │──│ seam│──│ Database │──│ seam│──│   API    │──→ ...        │
│  │  layer   │  │check│  │  layer   │  │check│  │  layer   │              │
│  │          │  │     │  │          │  │     │  │          │              │
│  │ Haiku    │  │ORM↔ │  │ Sonnet   │  │DB↔  │  │ Sonnet   │              │
│  │ 2 passes │  │ DB  │  │ 2 passes │  │Infra│  │ 4 passes │              │
│  └──────────┘  └─────┘  └──────────┘  └─────┘  └──────────┘              │
│                                                                             │
│  Each layer:  PICE Loop (Plan → Implement → Contract → Evaluate)           │
│  Each seam:   12 failure categories checked per boundary pair               │
│  Each eval:   Bayesian-SPRT + ADTS determines pass count                   │
└─────────────────────────────────────────────────────────────────────────────┘
              │
              ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                        ARCH EXPERTS (v0.3)                                   │
│           Dynamically generated from project architecture                    │
│                                                                             │
│  Architecture discovery ──→ Expert spawning ──→ Seam ownership              │
│                                                                             │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐                     │
│  │  RunPod      │  │  Docker      │  │  Next.js     │                     │
│  │  Expert      │  │  Expert      │  │  Expert      │                     │
│  │              │  │              │  │              │                     │
│  │ Provides:    │  │ Provides:    │  │ Provides:    │                     │
│  │  handler fmt │  │  image build │  │  SSR routes  │                     │
│  │ Assumes:     │  │ Assumes:     │  │ Assumes:     │                     │
│  │  env vars    │  │  registry    │  │  API < 200ms │                     │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘                     │
│         │                  │                  │                             │
│         └──────────────────┼──────────────────┘                             │
│                            ▼                                                │
│              Adversarial assumption mining:                                  │
│              Claude infers consumer assumptions                              │
│              GPT infers provider guarantees                                  │
│              PICE flags asymmetries = seam gaps                             │
└─────────────────────────────────────────────────────────────────────────────┘
              │
              ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    SELF-EVOLVING LOOP (v0.5)                                │
│             Every execution makes the next one smarter                       │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                    SQLite Metrics Engine                             │   │
│  │  Hit rates │ FPR │ Layer failures │ Cost │ Model agreement │ Escapes│   │
│  └──────────────────────────────────────┬──────────────────────────────┘   │
│                                          │                                  │
│         ┌────────────────────────────────┼────────────────────────┐         │
│         ▼                                ▼                        ▼         │
│  ┌──────────────┐  ┌──────────────────────────┐  ┌──────────────────┐     │
│  │Inner loop:   │  │Check value score:         │  │Outer loop:       │     │
│  │Tune params   │  │(hit×severity×(1-FPR))/cost│  │Evolve criteria   │     │
│  │• thresholds  │  │                            │  │• generate checks │     │
│  │• model tier  │  │Predictive selection:       │  │• retire obsolete │     │
│  │• budgets     │  │Run highest-value checks    │  │• restructure     │     │
│  │• prompts     │  │first, skip zero-value      │  │  seam model      │     │
│  └──────────────┘  └──────────────────────────┘  └──────────────────┘     │
│                                                                             │
│  Ground truth: evaluation-to-production correlation                         │
│  Do PICE verdicts predict production incidents? That's the signal.          │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Decision Flow: Novel Adaptive Verification Algorithms

```
                    Code change submitted
                           │
                           ▼
                ┌─────────────────────┐
                │  Initialize priors  │
                │  Beta(α₀, β₀) from │
                │  code complexity +  │
                │  historical rates   │
                └──────────┬──────────┘
                           │
                           ▼
               ┌──────────────────────┐
               │   PASS 1: Claude     │
               │   evaluates layer    │
               │                      │
               │   Update posterior:  │
               │   Beta(α₀+w₁, β₀)  │
               │   or Beta(α₀, β₀+w₁)│
               └──────────┬───────────┘
                          │
                          ▼
               ┌──────────────────────┐
               │   PASS 2: GPT       │
               │   evaluates layer    │
               │                      │
               │   Update posterior:  │
               │   Beta(α₁+w₂, β₁)  │
               │   or Beta(α₁, β₁+w₂)│
               └──────────┬───────────┘
                          │
                          ▼
               ┌──────────────────────┐
               │  ADTS: Compute      │
               │  divergence D₂      │
               │  between Claude     │
               │  and GPT verdicts   │
               └──────────┬───────────┘
                          │
            ┌─────────────┼─────────────┐
            ▼             ▼             ▼
     D₂ < τ_low    τ_low ≤ D₂     D₂ > τ_high
     (Agreement)    ≤ τ_high       (Disagreement)
            │        (Uncertain)          │
            ▼             │               ▼
    ┌───────────┐         │      ┌────────────────┐
    │  Check    │         │      │  Escalate:     │
    │  SPRT     │         │      │  Add 3rd model │
    │  thresholds│        │      │  (tiebreaker)  │
    │           │         │      └───────┬────────┘
    │  Λ ≥ A?  │         │              │
    │  → ACCEPT │         ▼              ▼
    │           │   ┌────────────┐  ┌────────────────┐
    │  Λ ≤ B?  │   │ Pass 3:    │  │ VEC: Compute   │
    │  → REJECT │   │ Targeted   │  │ semantic       │
    │           │   │ evaluation │  │ entropy SE₃    │
    └─────┬─────┘   │ on highest │  │                │
          │         │ divergence │  │ SE < ε AND     │
          ▼         │ dimensions │  │ |ΔSE| < δ?     │
    ┌───────────┐   └─────┬──────┘  │                │
    │  VERDICT  │         │         │ Yes → HALT     │
    │           │         ▼         │ No → Pass 4+   │
    │  PASS at  │   ┌────────────┐  └───────┬────────┘
    │  92.1%    │   │ Recompute  │          │
    │  conf.    │   │ ADTS + SPRT│          ▼
    │  (2 pass) │   │            │   ┌──────────────┐
    │           │   │ Converged? │   │ Decompose:   │
    │ ~70% of   │   │ → VERDICT  │   │              │
    │ all evals │   │            │   │ Epistemic?   │
    └───────────┘   │ Not yet?   │   │ → More passes│
                    │ → Pass 4   │   │              │
                    └────────────┘   │ Aleatoric?   │
                                     │ → Human      │
                     ~25% of         │   escalation │
                     all evals       └──────────────┘

                                      ~5% of
                                      all evals

    ┌─────────────────────────────────────────────────────┐
    │  CONFIDENCE CURVE (ρ = 0.35, p = 0.88)              │
    │                                                      │
    │  100%|                                               │
    │      |                              ceiling ~96.6%   │
    │  96% |─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─   │
    │      |                    ●─────●─────●──────        │
    │  94% |              ●                                │
    │      |         ●                                     │
    │  92% |    ●                                          │
    │      |                                               │
    │  88% |●                                              │
    │      |                                               │
    │      └──┬──┬──┬──┬──┬──┬──┬──┬──┬──┬──              │
    │         1  2  3  4  5  6  7  8  9  10  passes        │
    │                                                      │
    │  75% of improvement in passes 1–3                    │
    │  92% of improvement in passes 1–5                    │
    │  Beyond 5: diminishing returns → add orthogonal      │
    │  signals (tests, static analysis, formal methods)    │
    └─────────────────────────────────────────────────────┘
```

-----

## Claude Code Integration Architecture

PICE v0.2+ integrates with Claude Code's multi-agent systems as the execution substrate. This section documents the integration approach and constraints.

### Why subagents, not Agent Teams

| Property         | Subagents (chosen)                              | Agent Teams (rejected)                        |
| ---------------- | ----------------------------------------------- | --------------------------------------------- |
| Status           | Stable, always available                        | Experimental, requires feature flag + Opus    |
| Communication    | Parent→child→parent only                        | Peer-to-peer via file-based mailbox           |
| Context          | Isolated; only final result returns             | Fully independent per teammate                |
| Session resume   | Supported                                       | Not supported                                 |
| Token cost       | Lower (1 context window per subagent)           | 3–7× single session                          |
| Known issues     | None critical                                   | Race conditions, task status lag, no resume   |

### Integration path: CLI subprocess

PICE spawns `claude --bare -p` as a subprocess from Rust with `--output-format stream-json` and `--agents <json>`. This avoids SDK licensing concerns — the TypeScript SDK is proprietary, while direct CLI invocation is the same as any program invoking any CLI tool. The Python SDK (`claude-agent-sdk`) is MIT-licensed and available as an alternative if PICE adds a Python bridge.

### Cost control

- **Model tiering.** Haiku for simple checks, Sonnet for implementation, Opus for coordination.
- **`maxTurns` per subagent.** Caps loop iterations.
- **ADTS-driven pass allocation.** 70% of evaluations stop at 2 passes. Only 5% reach 5+.
- **Expert team sizing.** 2–3 specialists, not a swarm. Ensemble theory shows diminishing returns past 3.
- **Check value scoring.** The self-evolving loop deprioritizes low-value checks automatically.

> **Expanded research:** [Claude Agent SDK Licensing Analysis](research/sdk-licensing.md) — Python (MIT) vs. TypeScript (proprietary) SDKs, bundled CLI binary constraints, Anthropic Commercial ToS implications, PICE's open source compatibility approach.

-----

## Licensing and Open Source Compatibility

PICE is open source. Claude Code integration requires navigating a layered licensing structure.

| Component                    | License                           | PICE approach                    |
| ---------------------------- | --------------------------------- | -------------------------------- |
| Python SDK (`claude-agent-sdk`) | MIT                             | Optional dependency              |
| TypeScript SDK               | Proprietary (Commercial ToS)      | Not used (CLI subprocess instead)|
| Claude Code CLI binary       | Proprietary (all rights reserved) | Users install independently      |

1. **Claude Code is optional.** PICE works without it. The integration activates when the CLI is on the user's system.
2. **Users bring their own installation and API keys.** PICE doesn't distribute the CLI binary or proxy authentication.
3. **CLI subprocess avoids SDK licensing concerns.** No compile-time dependency on proprietary packages.
4. **No Anthropic trademarks.** PICE doesn't present itself as a Claude Code product.

-----

## Future Considerations

Ideas shaped by real usage and community feedback, not commitments.

**Community expert knowledge packs.** Technology-specific gotchas versioned by technology version ("RunPod v2.1 has a known issue with handler timeouts over 300s").

**Community seam pattern libraries.** Known failure patterns at specific technology boundaries (e.g., "Next.js API routes + Prisma connection pooling in serverless").

**Cross-layer dependency detection.** Automatically detect when a change in one layer implies changes in another.

**Parallel layer execution.** Independent layers could run PICE loops concurrently using `run_in_background: true`.

**Production traffic integration.** For v0.4's implicit contract inference, integrate with OpenTelemetry/Jaeger/Datadog to observe actual service behavior at boundaries.

**Agent Teams adoption (conditional).** If Claude Code's experimental Agent Teams feature stabilizes — session resume, race condition fixes, production-grade reliability — PICE could adopt it for parallel Tier 3 evaluation.

**Coordinator Mode.** Claude Code's `COORDINATOR_MODE` creates a pure orchestrator that relinquishes filesystem tools and exclusively manages workers — mapping closely to PICE's coordinator role. Monitor for stabilization.

-----

## Timeline

| Version | Focus                                                        | Status          |
| ------- | ------------------------------------------------------------ | --------------- |
| v0.1    | Core PICE loop, dual-model eval, metrics                     | ✅ Released      |
| v0.2    | Stack Loops (novel) + seam verification + adaptive algorithms | 🔬 Design phase  |
| v0.3    | Arch Experts (novel) + seam ownership + assumption mining     | 📋 Concept phase |
| v0.4    | Implicit Contract Inference (novel, no prior art)            | 🔭 Research phase |
| v0.5    | Self-Evolving Verification (novel closed-loop architecture)  | 🔭 Research phase |

We're building in the open. Follow progress in [Issues](https://github.com/jmolz/pice-framework/issues) and [Discussions](https://github.com/jmolz/pice-framework/discussions).

-----

## Research Library

| Document | Focus |
| --- | --- |
| [Stack Loops v0.2 Gap Analysis](research/v02-gap-analysis.md) | 37-gap analysis: layer detection, incremental re-eval, CI timing, deployment transitions, IaC modeling, crash recovery, onboarding |
| [The Seam Blindspot](research/seam-blindspot.md) | 23-category failure taxonomy, tooling gap analysis, cross-domain verification |
| [Convergence Analysis](research/convergence-analysis.md) | Correlated Condorcet Theorem, confidence curves, Bayesian-SPRT/ADTS/VEC derivations |
| [Self-Evolving Verification](research/self-evolving-verification.md) | MAPE-K, predictive test selection, DSPy optimization, evolutionary check generation |
| [Claude Code Integration](research/claude-code-integration.md) | Subagent architecture, Agent SDK, CLI subprocess integration, licensing |
| [Originality Analysis](research/originality-analysis.md) | Prior art search confirming Stack Loops and Arch Experts are novel terms |
| [SDK Licensing](research/sdk-licensing.md) | Python (MIT) vs. TypeScript (proprietary) SDKs, open source compatibility |
