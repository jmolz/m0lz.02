# Stack Loops v0.2: Critical Gap Analysis

*PICE Research Library — Expanded research supporting the [PICE Roadmap](../roadmap.md)*

*For term definitions, see the [Glossary](../glossary.md).*

-----

## Executive Summary

A systematic stress-test of the Stack Loops v0.2 design surfaced 37 distinct gaps across eight dimensions, with twelve classified as production-blocking. All twelve have been addressed as solved design decisions in the roadmap. This document provides the full analysis behind those decisions — the research, competitive intelligence, and reasoning that drove each resolution — serving as the technical record for why v0.2's architecture takes the shape it does.

The competitive landscape validates the thesis: no existing tool implements per-layer AI verification as of April 2026. But well-funded competitors — Qodo ($120M raised), SonarSource (AC/DC framework), Augment Code (Intent system) — could pivot within months. The market context creates urgency: 41% of all code is AI-generated, yet 96% of developers don't trust its accuracy.

-----

## 1. Layer Detection and Configuration

### The problem

No existing tool defines "layers" the way Stack Loops needs them:

- **Monorepo tools** (Nx, Turborepo, Bazel) organize by *projects* and *workspaces*, not architectural tiers
- **Package scanners** (Snyk, Dependabot, Renovate) detect *manifest files* and *dependency managers* — Renovate supports 100+ package managers across 30+ languages but produces dependency graphs, not layer maps
- **Framework detectors** (Heroku buildpacks, Nixpacks, Vercel) identify *runtimes* — Vercel detects 40+ frameworks but maps them to deployment targets, not architectural layers
- **Language detectors** (GitHub Linguist) identify *programming languages*, not how they're architecturally organized

None of these produce the "backend layer, database layer, API layer" decomposition that Stack Loops requires as input.

### Sub-gaps identified

**Gap 1.1: Fullstack-in-one frameworks.** Next.js, Remix, SvelteKit, and Nuxt combine frontend, API routes, and database access in a single codebase. A Next.js `pages/api/users.ts` importing `@prisma/client` spans three layers simultaneously. Layer boundaries are code paths, not directories.

**Gap 1.2: Monorepos with shared code.** In a 15-microservice monorepo, each service is its own stack. But shared libraries (auth utilities, TypeScript types, database models) are consumed by multiple stacks' layers and owned by none. Service-to-service dependencies create cross-stack seams distinct from within-stack layer seams.

**Gap 1.3: Polyrepos.** When the frontend is in one repository and the API in another, scanning a single repo reveals only part of the stack. Cross-repo layer detection is fundamentally unsolved. Nx's "synthetic monorepo" (cross-repo graphs via Nx Cloud) is experimental and requires explicit configuration.

**Gap 1.4: Dynamic dependencies.** Database connections via environment variables, service communication through service meshes (Istio, Linkerd), and message queues referenced only by connection strings — these seams exist at configuration time, not in code.

**Gap 1.5: Non-standard project structures.** Not every project follows conventional directory layouts. A Go project with everything in the root directory, a Python project with non-standard package names, or a legacy project with decades of accumulated structure all defeat convention-based heuristics.

### Resolution

PICE uses a **six-level heuristic detection stack** with mandatory manual override:

1. Manifest files → runtime, framework, dependencies
2. Directory patterns → conventional locations (app/, api/, infra/, deploy/)
3. Framework signals → framework-specific patterns (Next.js app/ routes, Prisma schema)
4. Config files → Docker, Terraform, CI/CD workflows
5. Import graph → static analysis of which files depend on which
6. Override file → `.pice/layers.toml` (always wins)

Fullstack frameworks use **file-level layer tagging**: files belong to multiple layers, each evaluated through a different contract lens. Monorepos are treated as multiple stacks with cross-stack seam checks on shared dependencies. Polyrepos are the acknowledged limitation — deferred to v0.4's distributed trace analysis for cross-repo seam inference, with `.pice/external-contracts.toml` for manual declaration in the interim.

Auto-detection generates a proposed `layers.toml` on `pice init`. The developer reviews, adjusts, and commits. This balances automation with human oversight — the human makes the architectural judgment call, PICE automates the verification that follows.

-----

## 2. Incremental Re-Evaluation

### The problem

When a layer fails and the developer fixes it, what happens to other layers?

- **Downstream invalidation** (standard): if the API layer's fix changes its response format, the frontend layer (which was verified against the old format) needs re-evaluation.
- **Upstream invalidation** (non-standard): if an infrastructure fix changes the database endpoint, the backend layer (which was verified assuming the old endpoint) needs re-evaluation.

Standard build systems — Nx, Turborepo, Bazel — all assume **unidirectional dependency flow**. Changes propagate downstream only. No production system handles reverse propagation.

### Research: how build systems handle this

**Bazel's change pruning** is the strongest existing primitive. When a dirty build target is rebuilt and its output is byte-identical to the previous version, Bazel stops propagating invalidation. This prevents unnecessary downstream rebuilds when internal changes don't affect the interface. Bazel's Skyframe evaluation framework models all build artifacts as nodes in a dependency graph with automatic invalidation tracking.

**Adapton** (PLDI 2014) introduces demand-driven change propagation — only recompute results when demanded by an observer, not eagerly. This avoids unnecessary computation when downstream layers haven't been queried yet.

**The pluto build system** (OOPSLA 2015) proved both soundness and optimality for incremental rebuilds with custom "stamps." Different build targets can use different staleness checks — schema hash for database layers, OpenAPI spec hash for API layers, content hash for code layers. This is directly applicable to Stack Loops, where each layer type has a natural "contract hash" that determines whether its interface has changed.

### Resolution

PICE implements a **bidirectional dependency graph with contract-based change pruning**:

- Forward edges model standard dependency flow (database → API → frontend)
- Each layer's verification is linked to the contract versions it consumes and produces
- When a fix changes a layer's produced contract (different contract hash), forward edges trigger downstream re-verification
- When a fix changes a layer's consumed contract (upstream layer's output changed), backward edges trigger upstream re-verification
- When a fix doesn't change any contracts (same hash), no propagation occurs — the most common case

The verification manifest tracks which contract version each layer was verified against. After a fix, PICE compares the new contract hash against the stored hash. In practice, most fixes don't change contracts — they fix implementation bugs that don't alter the interface. Contract-based pruning skips re-verification in the majority of cases.

-----

## 3. CI/CD Integration and Timing

### The problem

Sequential verification of all layers takes too long for CI/CD:

- 10 layers × 2–4 minutes each = 20–40 minutes sequential
- Developers context-switch away from CI after **6–7 minutes** (Honeycomb Engineering research)
- Each additional 5 minutes of CI time increases average time-to-merge by **over an hour** (Graphite data)
- Kent Beck's guidance: builds exceeding 10 minutes are "used much less often"

### Research: what's the acceptable ceiling?

Graphite's research across thousands of engineering teams found that CI time is the single strongest predictor of merge velocity. Their data shows:

- Under 5 minutes: optimal — developers stay engaged
- 5–10 minutes: acceptable — some context-switching but recoverable
- 10–20 minutes: degraded — significant productivity loss
- Over 20 minutes: broken — developers batch PRs, skip CI, or find workarounds

For AI-assisted CI specifically, a USENIX case study (September 2025) from Wiser Solutions documented that AI review steps need automatic disabling when responses exceed 30 seconds or costs exceed thresholds. Token consumption is substantial: Claude Code's simple "edit this file" command consumes 50,000–150,000 tokens per API call.

### Integration patterns

**GitHub App** (CodeRabbit/Qodo model): Zero per-repo config, webhook-triggered, posts inline PR comments. Best for adoption friction but requires data to leave the developer's environment.

**GitHub Action / CI step**: Full control, runs in developer's environment, but consumes CI minutes and requires secret management for AI API keys.

**Reusable workflows**: Each layer's verification becomes a modular, conditionally invoked workflow — the most architecturally clean option for layer-by-layer checks. GitHub Actions' `paths` filter + `needs` dependencies enable layer-aware conditional execution.

### Resolution

Four strategies keep total time under 10 minutes:

1. **Path-based filtering** (biggest impact): only verify layers whose files changed. `pice affected` computes the changed layer set from the git diff. A CSS-only change might verify only 2 of 7 layers.

2. **Parallel layer execution**: independent layers run concurrently. The dependency graph determines parallelization opportunities. Backend and frontend layers (no dependency edge) run simultaneously.

3. **Tiered model routing**: Haiku (~100ms response) for simple checks. Sonnet (~2s) for standard evaluation. Opus (~5s) only for Tier 3. Most Tier 1 evaluations complete in under 30 seconds per layer.

4. **Prompt caching**: Anthropic's prompt caching reduces costs by 90% and latency by 85% on repeated context. Layer contracts and system prompts are cached across runs — only changed code is new context.

Additionally, the Anthropic **Batch API** (50% cost reduction, 24-hour processing window) is available for non-interactive CI runs where immediate feedback isn't required — e.g., nightly full-stack evaluations.

**Cost circuit breakers** are implemented at three levels: per-layer (abort if single layer exceeds $X), per-evaluation (abort if total exceeds $Y), and per-billing-period (alert and optionally halt if monthly spend exceeds $Z). These are configured in `.pice/config.toml` and enforced by the Rust coordinator.

-----

## 4. Feature Flags and Deployment Strategies

### The problem: feature flags

With N boolean feature flags, a single layer exhibits 2^N possible behaviors. LaunchDarkly's documentation calls exhaustive testing of all combinations "clearly untenable." Martin Fowler's taxonomy distinguishes four flag types with different lifecycles and testing requirements:

- **Release toggles**: temporary, gate incomplete features
- **Experiment toggles**: A/B tests, need both variants verified
- **Ops toggles**: runtime circuit breakers, need failure-path verification
- **Permission toggles**: user-level feature access, need authorization verification

### The problem: deployment transitions

**Canary deployments** create two concurrent versions of a layer with traffic splitting. Both versions must satisfy contracts with the same upstream and downstream layers, but may expose different API schemas, performance characteristics, or feature sets. The "seam" becomes two parallel seams active simultaneously.

**Blue-green deployments** require database migrations compatible with both API versions during the transition window. The database layer's contract must simultaneously satisfy two different consumers — a constraint standard contract models don't express.

**Rolling deployments** create a window where N-1 instances run the old version and 1 instance runs the new version, potentially with different contract behaviors.

### Resolution: feature flags

Contracts are indexed by flag state with **pairwise coverage** rather than exhaustive testing:

```toml
[feature_flags]
new_auth_flow = { affects_layers = ["api", "frontend"], default = false }
```

Each flag combination is tested with at least one other flag variation, covering interaction effects without combinatorial explosion. The pairwise testing strategy reduces 2^N combinations to approximately N² — from 1,024 tests for 10 flags to ~100.

Contracts declare which flags affect which layers. Only affected layers are re-evaluated when a flag state changes. Flag-agnostic contract criteria (structural checks, security requirements) run regardless of flag state.

### Resolution: deployment transitions

PICE models deployment transitions with **version-aware seams**:

- Database migrations use the expand-and-contract pattern: schema changes are decomposed into additive (expand) and subtractive (contract) phases, each independently verifiable
- `pice evaluate --transition` explicitly tests both the current production version and the incoming version against shared contracts
- Seam compatibility checks verify that the old and new versions can coexist during the transition window
- After full cutover, transition-specific checks are automatically retired

-----

## 5. Infrastructure-as-Code Modeling

### The problem

The roadmap initially treated infrastructure as a peer layer alongside backend, database, and API. IaC is categorically different:

- It **creates** other layers (you can't have a database layer without IaC provisioning the database)
- It **defines** the seams (network policies and IAM roles determine whether API can reach the database)
- It **parameterizes** contracts (environment-specific configs change which contracts apply)
- Its verification is **slow** (terraform plan takes minutes), **expensive** (real cloud resources cost money), **non-deterministic** (cloud API availability varies), and **stateful** (must verify state over time)

### Resolution

IaC is modeled as a **meta-layer** with `type = "meta"` in `.pice/layers.toml`. Meta-layers have distinct semantics:

- **Provisioning seams** (IaC → application) are separated from **runtime seams** (API → database). Provisioning seams verify that infrastructure outputs match application inputs. Runtime seams verify operational behavior.

- **Tiered IaC verification** respects the cost/time constraints:
  - Tier 1: Static analysis only (terraform validate, tfsec, checkov) — seconds
  - Tier 2: AI evaluation of config correctness — minutes
  - Tier 3: Plan-based verification (terraform plan → evaluate diff) — minutes
  - Actual deployment testing is out of scope — that's staging

- **Multi-cloud** gets a two-dimensional model: layers × cloud providers. A single API layer on AWS and Azure has different IAM, networking, and failure modes. Contracts at each intersection are evaluated independently.

The IaC testing pyramid is well-established (Terraform's native test framework since 1.7, Terratest, Checkov, tfsec). PICE integrates with these tools rather than replacing them — the meta-layer contract can reference external tool outputs as verification evidence.

-----

## 6. Cross-Layer Contract Format

### The problem

Every existing contract format covers exactly one layer:

| Format | Layer coverage |
|---|---|
| OpenAPI | REST APIs |
| AsyncAPI | Event-driven interfaces |
| Protobuf / gRPC | RPC interfaces |
| Prisma / Drizzle schemas | Database |
| GraphQL SDL | Query interfaces |
| Terraform HCL | Infrastructure |

No format spans UI → API → Service → Data with consistent versioning and compatibility semantics.

### Research: format selection

**YAML** is the recommended primary format, validated by the precedent of OpenAPI, AsyncAPI, Kubernetes, and GitHub Actions:

- JSON lacks comments — disqualifying for developer-authored contracts
- TOML fails at deep nesting beyond 3 levels
- Markdown with structured sections requires custom parsers
- YAML with JSON Schema validation provides structure + readability + tooling

A hybrid approach — YAML for declaration with an optional TypeScript SDK for programmatic creation (following Spring Cloud Contract's Groovy DSL + YAML dual support) — provides the best balance.

### Contract versioning

Contracts adopt **Confluent Schema Registry's compatibility modes** applied to layer contracts:

- **BACKWARD**: new contract can read data produced by old contract
- **FORWARD**: old contract can read data produced by new contract
- **FULL**: both backward and forward compatible
- **TRANSITIVE** variants: compatibility across all historical versions, not just adjacent

Semantic versioning with explicit compatibility declarations enables automated checking. The expand-and-contract pattern for breaking changes provides verifiable intermediate states.

### Resolution

PICE defines a unified contract format in YAML with JSON Schema validation. The `failure_category` field links each check to the twelve empirically validated failure categories from the seam blindspot research. Contracts support environment-specific sections, feature flag indexing, and metadata tracking for auto-generation and manual refinement. See the roadmap's "Cross-layer contract format" section for the full schema.

Specmatic (using OpenAPI + AsyncAPI + gRPC proto as executable contracts, with an MCP server for Claude Code integration) is the closest existing tool. But Specmatic covers no database layer, no UI component contracts, no concept of chained layer-to-layer contracts, and no seam verification between layers. PICE's contract format fills this gap.

-----

## 7. Verification System Failure Modes

### Gap 7.1: Dual-provider outage

StatusGator has tracked 1,098+ Anthropic outages since June 2024. OpenAI has parallel outage history. Both providers experienced significant disruptions in the same weeks of March 2026. If Claude Code is the primary verifier and OpenAI the adversarial evaluator, a correlated outage blocks all verification.

**Resolution:** Four-tier graceful degradation:
- Tier A: Full AI verification (normal)
- Tier B: Single-model (one provider down)
- Tier C: Cached results for unchanged layers + static checks (both down)
- Tier D: Skip with prominent warning (emergency bypass)

Best practice from LLM reliability engineering: implement circuit breakers, timeouts, and retry budgets per provider. Graceful degradation is a well-established pattern — every major LLM-powered system implements it.

### Gap 7.2: Model version drift

Apple's MUSCLE research found that when pretrained LLM base models are updated, fine-tuned adapters experience "negative flips" — previously correct instances become incorrect. Verification prompts tuned for Claude Sonnet 4.5 may produce different results on 4.6.

**Resolution:** Three mechanisms:
1. Pinned model versions in config (e.g., `claude-sonnet-4-20250514`)
2. Golden evaluation regression suite (`.pice/golden-evaluations/`)
3. Consensus voting across old and new model versions for critical checks

### Gap 7.3: Token budget exhaustion

Anthropic's rate limits use a token bucket algorithm with per-minute quotas. A 500-file monorepo can exceed 500K tokens per request. Tier 1 API limits are approximately 30K input tokens per minute for Sonnet.

**Resolution:**
- Prompt caching (cached tokens don't count toward input TPM limits)
- Fresh conversations per layer (avoid context accumulation)
- Batch API (50% cost reduction, 24-hour window for non-interactive CI)
- Per-layer token budget limits in config
- Automatic retry with exponential backoff on rate limit errors

### Gap 7.4: Crash recovery

If PICE verifies 10 layers and crashes on layer 7, it must not re-verify layers 1–6.

**Resolution:** Verification manifest — a JSON checkpoint file recording completed layers, their contract hashes, model versions, and confidence scores. On resume, PICE reads the manifest, skips completed layers whose content hashes haven't changed, and continues from the last incomplete layer. Each layer verification is idempotent.

-----

## 8. Developer Experience and Onboarding

### The problem

Survey data from 202 open source developers shows:
- **34.2%** abandon a tool if setup is painful — the #1 abandonment trigger
- **17.3%** abandon due to bad documentation
- **12.4%** abandon due to missing features

The benchmark: one-command install or under 5 minutes of manual setup.

### Research: successful adoption patterns

The TypeScript adoption playbook is the template: start permissive (`allowJs` + `checkJs`), tighten gradually, support running alongside existing tools.

ESLint's recommended configs start loose and allow incremental strictness. Prettier adopts an opinionated-defaults-with-overrides model. Both achieve wide adoption by avoiding the "wall of errors on first run" problem.

Google's error message guidelines, validated by Stripe's API error UX and CLI best practices from 30+ production developer tools, converge on a rule: every error must answer "What went wrong?" AND "How do I fix it?"

### Resolution

**`pice init` (<5 minutes):** Auto-detects layers, generates `.pice/layers.toml` and `.pice/contracts/`, outputs a summary for human review. No manual configuration required to start.

**Baseline mode:** First evaluation runs with `--baseline` flag — reports findings without blocking. Establishes the current state of the codebase. Findings go to `.pice/baseline/` for gradual resolution. The team enables enforcement layer by layer as baseline findings are addressed.

**Actionable diagnostics:** Every failed check includes:
- The layer and check that failed
- The specific contract criterion violated
- The code location (file, line, function)
- A suggested fix (AI-generated)
- The confidence level
- Whether this is a seam check (and which boundary pair)

Low-confidence findings are explicitly marked — presenting uncertain findings as definitive erodes trust faster than not flagging them at all.

**Progressive strictness:** Teams start with Tier 1 (affected layers only, 2 passes, single evaluator) and graduate to Tier 2 and Tier 3 as confidence in the system grows. The `.pice/config.toml` makes tier selection explicit and auditable.

-----

## 9. Competitive Landscape (April 2026)

### No one ships per-layer AI verification today

This is validated across every competitor analyzed:

**Qodo** (formerly CodiumAI) — Raised $70M Series B in March 2026 for "code integrity." Generates tests and does "system-wide impact analysis" but does not decompose by architectural layer. Positions itself as "quality-first code gen" rather than post-generation verification. Could pivot toward layer awareness with their funding.

**SonarSource** — Introduced the "Agent Centric Development Cycle" (AC/DC) framework: Guide → Generate → Verify → Solve. The most conceptually similar to PICE — verification as a first-class concern in the AI coding loop. But SonarQube verifies by *code quality dimensions* (bugs, vulnerabilities, code smells), not *architectural layers*. No seam concept.

**IronBee** — "The Verification and Intelligence Layer for AI Coding Agents." Uses runtime tracing through 7 sequential verification phases. The closest structural analog to Stack Loops but focuses on runtime behavior verification rather than architectural seam verification. Pre-seed stage.

**Opslane** — "The Verification Layer for AI Code." Deploys PR branches in isolated containers for runtime testing. No per-layer decomposition, no seam concept.

**CodeRabbit** — AI code review that posts inline PR comments. Context-aware across the PR but no architectural layer model. Verifies code quality, not deployment readiness.

**Augment Code** — "Intent" system that evaluates whether generated code matches developer intent. Novel concept but operates at the intent→implementation gap, not the implementation→deployment gap.

### The market context

- **41% of all code in 2026 is AI-generated** (Anthropic, GitHub data)
- **96% of developers don't trust AI-generated code accuracy** (survey data)
- AI PRs contain **1.7× more issues** than human-written PRs (CodeRabbit)
- Technical debt increased **30–41%** after AI tool adoption (GitClear/multiple studies)
- EU AI Act becomes fully applicable for high-risk systems **August 2026**
- Werner Vogels coined **"verification debt"** as the defining challenge

Every major player — Anthropic, Sonar, Qodo, Augment — now frames verification as the critical bottleneck. The phrase has entered mainstream DevOps vocabulary. But nobody has built per-layer, seam-aware verification. PICE occupies this position alone.

### The window

Claude Code's infrastructure — skills framework, hooks, dispatch, and the `/batch` skill that decomposes work into 5–30 independent units — makes it the ideal substrate for Stack Loops. But Anthropic could add native layer-aware verification features at any time.

The recommendation: ship quickly, establish the conceptual vocabulary ("layers," "seams," "contracts," "Stack Loop"), and capture mindshare before well-funded players extend their verification capabilities.

-----

## 10. Summary: All Twelve Production-Breaking Gaps Resolved

| # | Gap | Resolution | Roadmap section |
|---|---|---|---|
| 1 | Layer detection has no foundation | Six-level heuristic + `.pice/layers.toml` override + `pice init` | Layer detection |
| 2 | Upstream invalidation unmodeled | Bidirectional graph + contract-hash change pruning | Incremental re-evaluation |
| 3 | Sequential timing is fatal | Path filtering + parallel execution + tiered routing + prompt caching | CI/CD integration |
| 4 | Feature flag combinatorial explosion | Flag-state-indexed contracts + pairwise coverage | Environment-specific contracts |
| 5 | Canary/blue-green breaks model | Version-aware seams + `--transition` flag + expand-and-contract | Deployment transitions |
| 6 | IaC is meta-layer, not peer | `type = "meta"` + provisioning seams + tiered IaC checks | Infrastructure-as-code |
| 7 | No crash recovery | Verification manifest (JSON checkpoint) + content-hash resume | Crash recovery |
| 8 | Dual-provider outage | Four-tier graceful degradation + circuit breakers | Resilience |
| 9 | Model version drift | Pinned versions + golden regression suite + consensus voting | Resilience |
| 10 | No cross-layer contract format | YAML + JSON Schema + failure_category taxonomy link | Contract format |
| 11 | Environment-specific variance | Invariant vs. environment-specific contract sections | Environment-specific contracts |
| 12 | Onboarding takes too long | `pice init` + baseline mode + actionable diagnostics + progressive strictness | Onboarding |

The remaining 25 non-blocking gaps (from the original 37) are categorized as enhancements, optimizations, or edge cases addressable in minor releases. None would prevent a team from adopting and benefiting from Stack Loops on a standard project.

-----

*See also: [Seam Blindspot](seam-blindspot.md) | [Convergence Analysis](convergence-analysis.md) | [Self-Evolving Verification](self-evolving-verification.md) | [Claude Code Integration](claude-code-integration.md) | [Glossary](../glossary.md)*
