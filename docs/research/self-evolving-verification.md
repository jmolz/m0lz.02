# Self-Evolving Verification Frameworks: State of the Art and Blueprint for PICE

*PICE Research Library — Expanded research supporting the [PICE Roadmap](../roadmap.md)*

*For term definitions, see the [Glossary](../glossary.md).*

-----

## Executive Summary

No production system today fully implements a closed-loop, self-evolving verification framework — one that genuinely learns from its own execution history to rewrite verification criteria, reallocate resources, and compound in value over time. But the building blocks exist across five distinct fields: predictive test selection (Meta, Google, Develocity), observability-driven development (Honeycomb, Tracetest), self-improving AI agents (Reflexion, DSPy, SICA), autonomic computing (MAPE-K), and evolutionary test optimization (EvoSuite). PICE's opportunity is to be the first system that integrates these patterns into a unified architecture where every evaluation makes the next one smarter, more targeted, and cheaper.

-----

## 1. Predictive Test Selection: Proof the Core Thesis Works at Scale

The strongest evidence that self-optimizing verification is viable comes from predictive test selection (PTS) systems deployed at Meta, Google, and Netflix. These systems track historical test outcomes, build ML models correlating code changes with test failures, and dynamically select which tests to run.

### Meta's PTS system

Published at ICSE-SEIP 2019. Uses a gradient-boosted decision tree trained on historical test results from their monolithic repository. The framing is the key innovation: rather than asking "which tests could be impacted?" (dependency analysis), it asks **"what is the probability this test finds a regression?"** — a fundamentally different question.

Results:
- Catches **>99.9% of faulty code changes** while running only one-third of transitively dependent tests
- Effectively doubles infrastructure efficiency
- The model retrains regularly on fresh results, adapting automatically as the codebase evolves

The feature set that drives predictions: which files changed, which tests historically fail on those files, recency of failures, developer identity, time of day, and commit metadata. This is directly analogous to the data PICE's SQLite metrics engine already collects — check outcomes, file associations, layer information, model used, evaluation confidence.

### Meta's Sapienz

Companion system using search-based software engineering to generate automated test cases. Key metric: **75% actionable report rate** — three-quarters of automated findings result in developer fixes. Attributed an **80% reduction in Android app crashes**. Demonstrates that automated verification can achieve production-quality signal, not just noise.

### Google's TAP platform

Processes **over 150 million test executions daily** across 4 billion individual test cases. Their ML-driven test selection reduced computational waste by over 30% while maintaining 99.9% regression safety confidence.

A surprising finding: algorithms based on the **number of distinct developers** committing code that triggers particular tests outperformed algorithms based on recent execution history. Social and organizational signals are unexpectedly powerful predictors of failure. This suggests PICE's metrics engine should consider who's making changes, not just what's changing.

### Develocity (formerly Gradle Enterprise)

Commercialized predictive test selection. Used by Netflix, LinkedIn, Airbnb. Netflix reported:
- **280,000 developer hours saved per year**
- Test execution times reduced from **10+ minutes to 1–2 minutes** (full order of magnitude)
- Maintained regression safety confidence

### Launchable (now CloudBees Smart Tests)

Demonstrated that running **20% of tests achieves 90% confidence** in catching failures, with models trained several times per week on fresh data. The 80/20 rule of verification — the minority of checks that catch the majority of issues can be identified from historical data.

### Direct lesson for PICE

The data PICE already collects in its SQLite metrics engine — check outcomes, file associations, layer information, model used, evaluation confidence — is precisely the feature set these systems use. The feedback loop is straightforward:

1. Train a model on historical check outcomes
2. For each new code change, predict which checks are most likely to catch issues
3. Run those checks first, skip checks with zero historical hit rate
4. Continuously retrain as new data arrives

The expected outcome: PICE runs fewer checks, catches the same or more issues, at lower cost and latency. The self-evolving loop makes this automatic.

-----

## 2. Observability-Driven Development

### Charity Majors and Honeycomb

Observability-Driven Development (ODD), championed by Charity Majors (CTO of Honeycomb), proposes a feedback loop extending verification beyond pre-deployment into production reality.

**Core thesis:** "Your job isn't done until it's in production." Deploying code is the beginning of gaining confidence, not the end. Majors advocates "two-window development" — code in one window, production telemetry in another, instrumenting as you go.

**Observability 1.0 vs. 2.0:**
- O11y 1.0: Separate pillars of metrics, logs, and traces in isolated tools. Pre-defined queries. Dashboard-centric.
- O11y 2.0: Unified storage of wide structured log events in columnar databases. Arbitrary high-cardinality slicing. You can slice by build ID, feature flag, user ID, or any dimension without pre-defining queries.

**Latest position (2026):** AI agents writing code need observability even more than humans, since their ability to validate changes determines the ROI of AI investment. This directly validates PICE's v0.5 architecture — the self-evolving loop needs production signals to close the feedback cycle.

### Tools closing the production-to-development loop

**Tracetest** (Kubeshop) — Creates assertions against distributed traces. Engineers turn production issues identified in Honeycomb into automated test assertions in CI/CD. Claims 80% reduction in troubleshooting time. The key innovation: tests are derived from observed production behavior, not hypothetical scenarios.

**Digma** — "Preemptive observability" as an IDE plugin. Uses OpenTelemetry instrumentation to surface runtime code insights (anti-patterns, bottlenecks, query issues) without requiring code changes. Detects problems at development time using runtime data from staging or production environments.

**Speedscale** — Captures production API traffic and auto-generates regression test suites from it. Replays sanitized traffic against new code versions. 2025 MCP integration lets AI coding agents pull exact failed production requests and replay them in sandboxes. This is essentially replay-based seam verification.

**Harness Continuous Verification** — The most mature product explicitly implementing production-data-driven verification. Queries health sources (Prometheus, Datadog, Splunk) automatically during deployment. Uses ML to learn normal behavior. Triggers **automatic rollback** when anomalies are detected.

### The key metric for PICE: evaluation-to-production correlation

The single most important signal for PICE's self-evolution: **do verification verdicts predict production incidents?**

Track two things:
1. Code that PICE passed → did it cause production incidents? (false negative rate)
2. Code that PICE flagged → would it have caused incidents if shipped? (true positive validation)

Over time, this correlation score becomes the ultimate ground truth for tuning the entire system. Checks that predict production issues get amplified. Checks that don't get deprioritized. The framework learns what actually matters — not what looked important in theory.

-----

## 3. Self-Improving AI Agent Architectures

### Reflexion (NeurIPS 2023)

Shinn et al. introduced verbal self-reflection: an agent attempts a task, observes failure, writes a natural-language critique stored in episodic memory, and retries conditioned on that feedback.

Results: **91% pass@1 on HumanEval** vs. GPT-4's 80%.

The key architectural insight is the **"semantic gradient"**: natural-language reflections stored in memory serve as a persistent, interpretable improvement signal. Unlike weight updates, these reflections are:
- Human-readable and auditable
- Persistent across sessions
- Composable (new reflections build on old ones)
- Reversible (bad reflections can be identified and removed)

For PICE: after each verification session, the framework can append discovered patterns ("this project's Docker builds fail when new dependencies aren't added to the multi-stage builder's first stage") to a persistent knowledge base. Future evaluations condition on this growing library.

### DSPy (Stanford NLP)

The state of the art in systematic prompt optimization from execution data. Rather than manually crafting prompts, you define:
- Input/output signatures (what goes in, what comes out)
- Metric functions (how to measure quality)
- A training set (examples of good and bad outcomes)

DSPy's optimizers automatically construct and refine prompts based on execution traces:

**MIPROv2** — Bootstraps traces from program runs, filters by metric score, drafts instructions grounded in program code and data, uses Bayesian optimization to search the instruction space.

**BootstrapFewShot** — Automatically selects the best few-shot examples from execution traces.

**SIMBA/GEPA** — More advanced optimizers for complex multi-step pipelines.

Reported gains: GPT-4o-mini scores from **66% to 87%** on classification tasks through automated prompt optimization alone.

For PICE: the Arch Expert system prompts and dual-model evaluation prompts are exactly the kind of structured LLM pipelines DSPy optimizes. Rather than manually tuning "You are a RunPod expert...", PICE can define metrics (did this expert catch real issues? what was its false positive rate?) and let DSPy-style optimization search the prompt space using accumulated evaluation traces.

### SICA — Self-Improving Coding Agent (ICLR 2025)

Goes further than Reflexion: SICA evaluates its own performance on benchmarks, then enters a self-edit phase where an LLM proposes modifications to the agent's **own source code** — prompts, heuristics, and architecture.

Key design:
- Maintains an **archive of previous agent versions** and their benchmark results
- Selects the best-performing variant as the "meta-agent" for the next improvement round
- Iterates through improvement cycles with version control

Results: **17–53% performance improvement** on SWE-Bench Verified.

For PICE: the concept of maintaining a versioned archive of verification configurations — including prompt versions, threshold settings, and check definitions — with tracked performance metrics per version. The system can test new configurations against historical data before deploying them.

### Darwin Gödel Machine (Sakana AI, 2025)

Extends SICA with open-ended evolutionary search. Automatically improved SWE-bench performance from **20.0% to 50.0%** through a growing archive of diverse agent variants. The most aggressive self-improvement approach: the system literally rewrites its own architecture.

For PICE: too aggressive for production verification (you don't want your safety system rewriting itself without guardrails), but validates the principle that automated self-improvement works. PICE can apply the concept with human-in-the-loop oversight — propose improvements, simulate against historical data, deploy with monitoring.

### The AGENTS.md / CLAUDE.md pattern

The most practical, widely-adopted approach for coding agents to learn without weight updates. A persistent markdown file accumulates:
- Patterns discovered during execution
- Gotchas specific to this project
- Conventions the agent should follow
- Mistakes to avoid

After each task, learnings are appended. Future iterations ingest this file. This four-channel memory approach (git history, progress log, task state, knowledge base) is simple but effective.

For PICE: the equivalent is `.pice/learnings.md` or `.pice/knowledge.md` — a growing file the framework reads and appends to across executions. Over time, it accumulates project-specific verification intelligence: "this project's Docker builds always need the builder stage to include openssl-dev," "the RunPod handler timeout needs to be 2x the p99 latency of the ML model."

-----

## 4. The MAPE-K Control Loop

### Architecture

IBM's MAPE-K loop (Monitor → Analyze → Plan → Execute, over a shared Knowledge base) is the canonical reference architecture for self-adaptive systems, introduced by Kephart & Chess in 2003. Over 6,000 research papers cite it.

**Monitor** — Collects raw telemetry from the managed system. For PICE: per-check pass/fail, confidence scores, token counts, latency, model used, environmental context (file changed, layer, component, developer).

**Analyze** — Processes raw data into actionable insights. For PICE: rolling averages, trend detection, statistical process control, Bayesian updating of check effectiveness, anomaly detection (sudden changes in failure patterns).

**Plan** — Generates adaptation decisions. For PICE: which checks to enable/disable, which model to assign per check, prompt refinement candidates, budget allocation across tiers.

**Execute** — Applies changes to the managed system. For PICE: update `.pice/config.toml`, adjust model routing, deploy new prompt versions, modify thresholds.

**Knowledge** — Shared data store accessible to all phases. For PICE: the SQLite metrics engine plus `.pice/learnings.md`.

### Recent critiques and extensions

**"Breaking the Loop: AWARE is the New MAPE-K"** (FSE 2025) argues the sequential, reactive, centralized MAPE-K loop struggles with modern complex systems — lacking proactivity, scalability, and continuous learning integration. The AWARE framework proposes replacing the loop with an event-driven, distributed architecture.

For PICE: the critique is valid for runtime systems but less applicable to verification frameworks where sequential processing is acceptable. However, the proactivity critique applies — PICE should not just react to failures but proactively predict which checks will be most valuable for each change.

**LLM-enhanced MAPE-K** (ECSA 2025) proposes integrating LLM-based agentic AI for the Analyze and Plan phases. The LLM handles natural-language reasoning about why patterns are emerging and what adaptation strategies might work.

For PICE: this is already the architecture. PICE's AI evaluators serve as both the managed system AND the Analyze/Plan intelligence. The self-evolving loop uses the same AI capabilities that perform verification to also reason about how to improve verification.

### Control theory foundations

Cangussu et al. developed a closed-loop feedback control model of the software test process grounded in automatic control theory. Key concepts that apply to PICE:

**Setpoints** — Target values the system maintains. For PICE: target FPR < 5%, cost per evaluation < $X, confidence > 95% for Tier 2.

**Error signals** — Difference between current and target metrics. For PICE: current FPR is 12% vs. target 5% → error signal of 7%.

**Controller gain** — How aggressively the system responds to error signals. Too high → oscillation (checks flip between enabled and disabled). Too low → slow adaptation. PICE should use conservative gain with damping.

**Stability margins** — Preventing harmful oscillations. PICE should require metrics to be consistently outside target for N consecutive evaluation cycles before adapting, preventing noise-driven changes.

**Dead bands** — Minimum error thresholds to prevent constant small adjustments. If FPR is 5.1% vs. target 5.0%, don't adapt — that's within noise.

-----

## 5. Double-Loop Learning

### Single-loop vs. double-loop

Chris Argyris and Donald Schön (1978) distinguished two modes of organizational learning:

**Single-loop learning** adjusts actions within existing rules. The thermostat maintains 68°F — if it's too cold, turn on the heat; if too warm, turn it off. The goal is never questioned.

**Double-loop learning** questions the rules themselves. Why 68°F? Should it be 72°F in winter and 66°F in summer? Should we use a thermostat at all, or a more sophisticated climate control system?

### Application to PICE

**Inner loop (single-loop):** Adjusts parameters within existing verification criteria.
- Tune thresholds: change the Bayesian-SPRT acceptance boundary from 0.95 to 0.93
- Reassign models: route this check type to Haiku instead of Sonnet (same accuracy, lower cost)
- Adjust budget: allocate more passes to infrastructure layer (higher failure rate)
- Refine prompts: modify Arch Expert system prompt based on DSPy optimization

**Outer loop (double-loop):** Questions the verification criteria themselves.
- Generate new checks: "this project has had 3 incidents from Docker networking issues → add a Docker network connectivity seam check"
- Retire obsolete checks: "this check hasn't fired in 180 days and costs $0.02/run → disable"
- Restructure the seam model: "this project's architecture has evolved — the API layer now communicates directly with the queue, bypassing the backend → add an API↔Queue seam"
- Evolve the tier structure: "Tier 1 is catching only 85% of issues for this project → expand Tier 1 scope"

The outer loop is triggered by:
- Sustained metric degradation (defect escape rate rising over 3 consecutive sprints)
- Pattern analysis (3+ incidents from the same failure category in 30 days)
- Architecture change detection (new files matching technology patterns not in the current seam model)
- Manual trigger (`pice evolve`)

-----

## 6. Evolutionary Test Optimization

### EvoSuite and EvoSuiteFIT

EvoSuite uses genetic algorithms to generate whole test suites optimizing for code coverage. The evolutionary process:
1. Initialize a population of random test suites
2. Evaluate fitness (coverage, mutation score)
3. Select, crossover, mutate
4. Iterate until convergence

EvoSuiteFIT extends this with **reinforcement-learning-based adaptive fitness function selection** — dynamically adjusting which optimization criteria drive the evolutionary search based on the current population's characteristics. The algorithm learns which fitness functions are most productive at each stage of evolution.

### EvoGPT (2025)

Hybridizes LLM test generation with evolutionary search:
1. LLMs generate diverse initial test suites (exploiting semantic understanding)
2. Genetic algorithm refines through selection, crossover, and mutation (exploiting systematic optimization)
3. Outperforms either approach alone

For PICE: the concept of evolutionary check generation. Generate candidate checks using AI, then evaluate their fitness (hit rate, FPR, cost, value score) over a probation period, and evolve the check population using selection pressure from real-world outcomes.

### DeepVerifier (2025)

Self-evolving verification agents that iteratively verify outputs using rubrics derived from an automatically constructed failure taxonomy. Outperformed agent-as-judge baselines by **12–48% in meta-evaluation F1 score**.

Key principle: **exploit the asymmetry of verification** — checking correctness is easier than generation. PICE's entire architecture is built on this asymmetry: the evaluation agents are simpler and cheaper than the implementation agents, but the verification framework that orchestrates them adds the compound value.

### ReVeal (2025)

Multi-turn RL framework for self-evolving code agents where generation and verification capabilities co-evolve through iterative execution feedback. Demonstrates self-improvement across **up to 19 inference turns** despite being trained with only 3. Proves that the iterative loop structure itself drives improvement beyond what training provides.

### Weaver (Stanford/UW-Madison/Together AI, 2025)

Closes the generation-verification gap using ensemble verification with learned verifier reliabilities: 30+ verifiers with probabilistic aggregation achieve **91% collective accuracy** when 20+ agree, despite individual accuracy of 43–62%.

Direct validation of PICE's dual-model approach — and suggests expanding to more evaluators with learned reliability weights. The ensemble's power comes not from count but from diversity and calibrated weighting.

### The Generator-Verifier-Updater framework

Chojecki (2025) unified self-play approaches under a Generator-Verifier-Updater (GVU) operator, showing that STaR, SPIN, Reflexion, GANs, and AlphaZero are specific topological realizations of the same fundamental pattern. This suggests PICE's dual-model adversarial setup is an instance of a **deeply general self-improvement mechanism** — not a specific technique but a manifestation of a universal pattern.

-----

## 7. Minimum Viable Telemetry for PICE's Closed Loop

### Phase 1: Seven core metrics (minimum viable)

These metrics enable basic predictive selection and parameter tuning:

| # | Metric | Collection method | Update frequency |
|---|--------|-------------------|------------------|
| 1 | **Per-check hit rate** | Count FAIL verdicts / total runs, rolling 30-day window | Per evaluation |
| 2 | **Per-check false positive rate** | Manual review sample or production correlation | Weekly batch |
| 3 | **Per-layer failure distribution** | Aggregate check outcomes by layer | Per evaluation |
| 4 | **Cost per evaluation** | Token count × model pricing, per model | Per evaluation |
| 5 | **Evaluation latency** | Wall-clock time p50, p95, p99 | Per evaluation |
| 6 | **Model agreement rate** | Cohen's kappa for dual-model checks | Per evaluation |
| 7 | **Defect escape rate** | Production incidents ÷ PASS verdicts | Weekly/sprint |

### Phase 2: Self-optimization signals (five additional metrics)

These enable automated adaptation:

| # | Metric | Formula | Purpose |
|---|--------|---------|---------|
| 8 | **Check value score** | (hit_rate × severity × (1−FPR)) / cost | Prioritize high-value checks |
| 9 | **Information weight** | Contribution to constraining the evaluation space | Identify redundant checks |
| 10 | **Trend detection** | Slope of rolling metric windows | Early warning of degradation |
| 11 | **Cost per true positive** | Total cost ÷ true positives | Primary ROI metric |
| 12 | **Predictive validity** | Correlation(verdict, production_outcome) | Ground truth calibration |

### Phase 3: Automated decision rules

Concrete rules the MAPE-K loop applies:

- **Auto-disable:** Checks with zero hit rate and cost > $0.01/run for >90 consecutive days
- **Auto-tier:** Route checks to cheaper models when accuracy is equivalent (Haiku vs. Sonnet)
- **Auto-alert:** When FPR exceeds 20% for any check, flag for human review
- **Auto-adjust:** Bayesian-SPRT thresholds based on precision-recall tradeoff curves
- **Budget guardrails:** Alert at 50%, 90%, 100% of evaluation budget allocation
- **Confidence floor:** Never auto-accept below configured minimum confidence (default 85%)

### SQLite schema recommendation

The metrics engine centers on an event-sourced `evaluations` table:

```sql
CREATE TABLE evaluations (
    id          TEXT PRIMARY KEY,    -- UUIDv7 for time-ordered IDs
    timestamp   TEXT NOT NULL,       -- ISO 8601
    feature_id  TEXT NOT NULL,       -- Links to feature/plan
    layer       TEXT NOT NULL,       -- 'backend', 'infrastructure', etc.
    check_id    TEXT NOT NULL,       -- Specific check identifier
    check_type  TEXT NOT NULL,       -- 'layer', 'seam', 'expert'
    model       TEXT NOT NULL,       -- 'haiku', 'sonnet', 'opus', 'gpt-4o'
    verdict     TEXT NOT NULL,       -- 'pass', 'fail', 'inconclusive'
    confidence  REAL NOT NULL,       -- 0.0–1.0 posterior probability
    tokens_in   INTEGER NOT NULL,
    tokens_out  INTEGER NOT NULL,
    cost_usd    REAL NOT NULL,
    latency_ms  INTEGER NOT NULL,
    pass_number INTEGER NOT NULL,    -- Which pass in the sequence (1, 2, 3...)
    tier        INTEGER NOT NULL,    -- ADTS tier (1, 2, 3)
    divergence  REAL,                -- ADTS divergence score D_n
    entropy     REAL,                -- VEC semantic entropy SE_n
    files_json  TEXT,                -- JSON array of affected files
    metadata    TEXT                  -- JSON blob for extensibility
);

CREATE INDEX idx_eval_layer ON evaluations(layer, timestamp);
CREATE INDEX idx_eval_check ON evaluations(check_id, timestamp);
CREATE INDEX idx_eval_feature ON evaluations(feature_id);
CREATE INDEX idx_eval_model ON evaluations(model, check_id);
```

Materialized rollup views at hourly/daily/weekly granularity:

```sql
CREATE TABLE check_rollups (
    check_id    TEXT NOT NULL,
    period      TEXT NOT NULL,       -- '2026-04-05', '2026-W14', '2026-04'
    period_type TEXT NOT NULL,       -- 'day', 'week', 'month'
    total_runs  INTEGER NOT NULL,
    pass_count  INTEGER NOT NULL,
    fail_count  INTEGER NOT NULL,
    hit_rate    REAL NOT NULL,
    avg_cost    REAL NOT NULL,
    avg_latency REAL NOT NULL,
    avg_confidence REAL NOT NULL,
    value_score REAL,
    PRIMARY KEY (check_id, period, period_type)
);
```

A configuration history table for traceability:

```sql
CREATE TABLE config_changes (
    id          TEXT PRIMARY KEY,
    timestamp   TEXT NOT NULL,
    change_type TEXT NOT NULL,       -- 'threshold', 'model_route', 'check_enable',
                                    -- 'check_disable', 'prompt_update'
    check_id    TEXT,
    old_value   TEXT,
    new_value   TEXT,
    reason      TEXT NOT NULL,       -- 'auto:low_hit_rate', 'auto:cost_optimization',
                                    -- 'manual:user_request'
    metrics_snapshot TEXT            -- JSON of metrics at time of decision
);
```

SQLite with WAL mode and proper indexing handles millions of rows with zero operational overhead — sufficient until data volume exceeds tens of gigabytes.

-----

## 8. The Complete Self-Evolving Architecture

Synthesizing all prior art, PICE's evolution from v0.1 metrics engine to a genuinely self-evolving verification framework combines five proven patterns:

### Pattern 1: MAPE-K skeleton

The control loop providing Monitor → Analyze → Plan → Execute over the SQLite Knowledge base. The inner loop adjusts parameters continuously. The outer loop, triggered by sustained metric degradation or pattern analysis, evolves the verification criteria themselves.

### Pattern 2: DSPy-style prompt optimization

Systematically improves Arch Expert and evaluation prompts. Define metric functions (accuracy, precision, cost). Let optimizers search the instruction space using accumulated evaluation traces. Each optimization cycle produces candidate prompts; simulation mode evaluates against historical data before deployment.

### Pattern 3: AGENTS.md / knowledge base pattern

Lightweight, interpretable learning. After each verification session, append discovered patterns (new anti-patterns, false positive triggers, effective prompt formulations, project-specific gotchas) to `.pice/learnings.md`. Future evaluations condition on this growing library. Transparent, auditable, reversible.

### Pattern 4: Ensemble verification with learned reliability weights

Extend the dual-model approach following Weaver. Track per-model, per-check-type accuracy and confidence calibration. When models disagree, weight their verdicts by learned reliability rather than treating them equally. Over time, route each check type to the model combination that maximizes the check value score.

### Pattern 5: Evolutionary check generation

Close the ultimate loop. Analyze patterns in historical failures — which architectural boundaries fail most, what code patterns trigger violations, what new violation types emerge. Generate candidate verification checks using AI. Enter probation period with tracked metrics. Promote checks that prove value; prune those that don't. This is EvoSuite's genetic algorithm concept applied to verification criteria instead of test cases.

### The compound value proposition

Each execution generates:
- Training data for check prioritization (predictive selection)
- Feedback for prompt optimization (DSPy-style)
- Signal for model routing (learned reliability weights)
- Evidence for threshold tuning (Bayesian prior updating)
- Candidates for check evolution (pattern analysis)
- Ground truth for system calibration (production correlation)

This is what it means for a framework to compound in value. Execution 1 is generic. Execution 100 is calibrated to your project. Execution 1,000 is a deeply specialized verification engine that knows your architecture's specific failure modes, your team's common mistakes, and which checks provide the most value per dollar.

-----

## 9. What No One Has Built Yet

Despite rich prior art in each individual domain, no system combines:

| Capability | Nearest existing system | What it lacks |
|---|---|---|
| Predictive check selection | Meta PTS, Develocity | Applied to traditional tests, not AI evaluation |
| Prompt optimization from execution data | DSPy | Applied to general LLM pipelines, not verification |
| Versioned configuration archive | SICA | Applied to coding agents, not verification frameworks |
| Production correlation feedback | Harness CV | Detects anomalies, doesn't feed back into verification criteria |
| Evolutionary criterion generation | EvoSuite | Generates test cases, not verification criteria |
| **All of the above in one system** | **Nothing** | **PICE v0.5** |

The gap is clear. The building blocks are mature. The integration is the novel contribution.

-----

*See also: [Seam Blindspot](seam-blindspot.md) | [Convergence Analysis](convergence-analysis.md) | [Claude Code Integration](claude-code-integration.md) | [Glossary](../glossary.md)*
