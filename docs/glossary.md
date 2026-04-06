# PICE Glossary

A comprehensive reference for every technical term, acronym, framework-specific concept, and research reference used across the PICE roadmap and research library. Terms are grouped by domain, then alphabetical within each group.

-----

## PICE Framework Concepts

**ADTS (Adversarial Divergence-Triggered Scaling)** — A novel PICE algorithm that uses the degree of disagreement between evaluator models (e.g., Claude vs. GPT) to dynamically determine how many evaluation passes a piece of code needs. Low disagreement → stop early (2 passes). High disagreement → escalate to more passes or human review. See [Convergence Analysis](research/convergence-analysis.md).

**Arch Experts** — A novel concept coined in PICE (v0.3). Dynamically generated specialist AI agents inferred from a project's actual architecture files (package.json, Dockerfile, docker-compose.yml, etc.). Unlike pre-built agent libraries, Arch Experts emerge automatically from your configuration — no manual selection needed. Each expert owns both a component and the seams around it.

**Bayesian-SPRT Adaptive Halting** — A novel PICE algorithm combining Bayesian belief updating (tracking a probability distribution over "is this code correct?") with Wald's Sequential Probability Ratio Test (a mathematically optimal stopping rule). After each evaluation pass, the system updates its confidence and checks whether it has enough evidence to accept or reject. See [Convergence Analysis](research/convergence-analysis.md).

**Check Value Score** — A composite metric computed as `(hit_rate × severity_weight × (1 − FPR)) / cost_per_run`. Enables direct comparison of how much value each verification check provides per dollar spent. Drives automated optimization in the self-evolving loop.

**Contract** — In PICE, a structured set of evaluation criteria that code must satisfy to pass a given layer. Contracts are both layer-specific (infrastructure contract, deployment contract) and seam-specific (API↔Frontend contract). Arch Experts inject technology-specific criteria into contracts.

**Double-Loop Learning** — A concept from organizational theory (Argyris & Schön, 1978) applied to PICE's self-evolution. The inner loop adjusts parameters within existing rules (like a thermostat maintaining temperature). The outer loop questions the rules themselves — whether verification criteria are correct, whether new checks are needed, whether the architectural model still reflects reality.

**Evaluation Pass** — A single round of AI model evaluation against a contract. PICE's adaptive algorithms determine how many passes each layer needs (typically 2–5) based on accumulated evidence and inter-model agreement.

**Implicit Contract** — A behavioral property that a system depends on but no one has documented, no schema enforces, and no test verifies. Examples: "this endpoint always returns within 200ms," "this field is never null even though the schema says optional," "this service starts before that one." PICE v0.4 aims to discover these automatically.

**Layer** — A horizontal slice of the technology stack that gets its own PICE loop. Default layers include Backend, Database, API, Frontend, Infrastructure, Deployment, and Observability. Configurable per project in `.pice/layers.toml`.

**PICE Loop** — The core Plan → Implement → Contract-Evaluate cycle. In v0.1, one loop per feature. In v0.2+, one loop per layer per feature (Stack Loops).

**Seam** — The integration boundary between two adjacent layers or components. Where Component A connects to Component B. The place where assumptions on one side may not match guarantees on the other. The primary failure point in software systems.

**Seam Check** — A verification step that runs at a layer boundary, checking whether the integration contract between adjacent layers holds. Seam checks are generated from the twelve empirically validated failure categories and filtered by which categories apply to each specific boundary pair.

**Seam Gap** — A discovered asymmetry where a consumer assumes something about a provider that the provider doesn't guarantee. Example: the API layer assumes responses arrive within 200ms, but the RunPod handler takes 400ms under cold start. PICE's adversarial assumption mining detects these.

**Stack Loops** — A novel concept coined in PICE (v0.2). Per-layer verification loops across the technology stack, where a feature is only complete when every layer passes. Each layer runs its own Plan → Implement → Contract-Evaluate cycle with layer-specific contracts and seam checks between layers.

**Tier** — PICE's evaluation depth levels, scaled to change significance. Tier 1: affected layers only, 2 passes, single evaluator. Tier 2: affected + always-run layers, 2–5 passes, dual-model. Tier 3: all layers, 3–10 passes, agent team + adversarial + formal verification.

**VEC (Verification Entropy Convergence)** — A novel PICE algorithm that stops evaluation when the information content of accumulated reviews converges — i.e., when additional passes stop adding meaningful new information. Based on semantic entropy measurement and epistemic/aleatoric uncertainty decomposition. See [Convergence Analysis](research/convergence-analysis.md).

-----

## AI and Multi-Agent Systems

**Agent Teams** — An experimental Claude Code feature (since v2.1.32, February 2026) that creates peer-to-peer teams of Claude Code instances communicating via file-based mailboxes. PICE does not use this due to instability (race conditions, no session resume). Not to be confused with subagents.

**AgentDefinition** — A TypeScript/JSON object in the Claude Agent SDK that defines a specialized agent's system prompt, allowed tools, model assignment, and other parameters. PICE constructs these dynamically at runtime for Arch Experts.

**Claude Code** — Anthropic's agentic AI coding tool, distributed as `@anthropic-ai/claude-code` on npm. PICE uses it as an optional execution substrate via CLI subprocess invocation.

**Coordinator Mode** — A Claude Code feature flag (`COORDINATOR_MODE`) that creates a pure orchestrator with no filesystem/shell tools, exclusively managing workers via `Agent` tool calls. Maps closely to PICE's coordinator role.

**CrewAI** — An open-source multi-agent framework using role-based agent specialization. Requires manual agent selection. PICE's Arch Experts are differentiated by being dynamically generated from project files.

**DSPy** — A Stanford NLP framework for systematic prompt optimization. Instead of manually crafting prompts, you define input/output signatures and metrics; DSPy's optimizers automatically search the instruction space using execution traces. PICE v0.5 could use DSPy-style optimization for Arch Expert prompts.

**LangGraph** — A LangChain library for building stateful, multi-actor applications with LLMs using cyclical graphs. Identifies four foundational multi-agent patterns: subagents, skills, handoffs, and routers.

**MCP (Model Context Protocol)** — A protocol for connecting AI models to external tools and services. Claude Code uses MCP servers to access tools like Sentry, databases, etc. PICE's Arch Experts can leverage MCP connections via the `mcpServers` field in agent definitions.

**MetaGPT** — A multi-agent framework simulating a software company with fixed roles (Product Manager, Architect, Engineer, etc.). Includes a static "Architect" role, unlike PICE's dynamically generated Arch Experts.

**Reflexion** — An AI agent architecture (Shinn et al., NeurIPS 2023) where agents attempt tasks, observe failures, write natural-language self-critiques stored in memory, and retry with that feedback. Achieved 91% on HumanEval vs. GPT-4's 80%.

**RLHF (Reinforcement Learning from Human Feedback)** — A technique for training AI models using human preference data. Not directly used in PICE, but related to the broader concept of learning from evaluation feedback.

**SICA (Self-Improving Coding Agent)** — An AI agent (ICLR 2025) that evaluates its own benchmark performance and proposes modifications to its own source code. Achieved 17–53% improvement on SWE-Bench Verified. Relevant to PICE's self-evolving loop concept.

**Subagent** — In Claude Code, an isolated child process spawned via the `Agent` tool with its own context window. Runs to completion; only the final result returns to the parent. Cannot spawn its own subagents. This is the execution mechanism PICE uses for Stack Loop evaluation.

-----

## Mathematics and Statistics

**Aleatoric Uncertainty** — Irreducible uncertainty arising from inherent randomness or ambiguity in the problem itself. In PICE: the specification is genuinely ambiguous, and no amount of additional model passes can resolve it. The correct response is to escalate to human review.

**Alpha Spending Function** — In group sequential trial design, a method for distributing the overall Type I error rate (α) across multiple interim analyses. PICE uses O'Brien-Fleming-style alpha spending to set stringent thresholds for early passes, preserving discriminative power for later ones.

**Bayesian Posterior** — A probability distribution representing updated beliefs after observing data. In PICE: a Beta distribution over P(code_correct), updated after each evaluation pass. Beta(α, β) where α counts weighted approvals and β counts weighted flags.

**Beta Distribution** — A probability distribution on [0, 1], commonly used as a prior/posterior for binary outcomes. PICE uses Beta(α₀, β₀) as the prior on code correctness, updated to Beta(α₀ + approvals, β₀ + flags) after evaluation.

**Chernoff Bound** — A mathematical inequality providing exponentially decreasing bounds on tail probabilities. Under independence: ε(N) ≤ exp(−N · D(0.5 ∥ 1−p)). Under correlation, the bound weakens significantly.

**Cohen's Kappa (κ)** — A statistic measuring inter-rater agreement for categorical items, accounting for chance agreement. PICE uses it to measure dual-model agreement rate — how often Claude and GPT reach the same verdict beyond what chance would predict.

**Condorcet Jury Theorem** — A mathematical result (1785) proving that majority-vote accuracy approaches 100% as the number of independent voters grows, provided each voter is more accurate than chance. The critical caveat: it assumes independence. When evaluators are correlated (as LLMs are), the theorem hits a ceiling.

**D_KL (Kullback-Leibler Divergence)** — A measure of how one probability distribution differs from another. In PICE's Bayesian-SPRT: the KL divergence between the "code is correct" and "code is defective" observation distributions determines the expected number of passes needed.

**Effective Sample Size (n_eff)** — The number of independent observations equivalent to a set of correlated observations. Formula: n_eff = n / (1 + (n−1)ρ). With ρ = 0.3 and n = ∞, n_eff caps at ~3.3 — meaning correlated LLM evaluators can never provide more than 3.3 independent opinions regardless of pass count.

**Epistemic Uncertainty** — Uncertainty arising from lack of knowledge, reducible by gathering more information. In PICE: the models don't understand the code well enough. The correct response is additional evaluation passes with diverse evaluators.

**Fisher Information** — A measure of how much information an observation carries about an unknown parameter. In PICE's VEC algorithm: I_i(θ) determines which evaluation dimensions provide the most information at the current quality estimate, enabling adaptive selection of what to evaluate next.

**IRT (Item Response Theory)** — A psychometric framework modeling the probability of a correct response as a function of person ability and item characteristics (difficulty, discrimination). PICE adapts IRT concepts for adaptive check selection — each "check" has difficulty and discrimination parameters, and the system selects checks that maximize information at the current quality estimate.

**Jensen-Shannon Divergence (JSD)** — A symmetric measure of similarity between two probability distributions. PICE's ADTS algorithm can use JSD to quantify the degree of disagreement between Claude and GPT evaluation distributions.

**Krogh-Vedelsby Decomposition** — A result from ensemble learning: E_ensemble = E_avg − Ambiguity. Ensemble error improves only to the extent that individual estimators disagree ("ambiguity"). Directly justifies PICE's use of diverse Arch Experts — specialist diversity is mathematically necessary for ensemble improvement.

**Log-Likelihood Ratio (Λₙ)** — In sequential testing, the cumulative ratio of likelihoods under two competing hypotheses. PICE's SPRT compares Λₙ against acceptance threshold A = (1−β)/α and rejection threshold B = β/(1−α). When Λₙ crosses a threshold, the test terminates.

**O'Brien-Fleming Boundaries** — A group sequential design that uses very stringent significance thresholds for early interim analyses (e.g., z ≥ 4.56 at first look in a 5-look design) and relaxes toward nominal α at the final analysis. Prevents premature acceptance of subtly flawed code while allowing rapid rejection of obviously broken submissions.

**PROMIS CAT** — Patient-Reported Outcomes Measurement Information System, Computerized Adaptive Testing. A health measurement system that adaptively selects questions and stops when the Standard Error drops below a threshold (typically SE < 0.3, requiring 4–12 items). PICE adapts this stopping rule for verification passes.

**Semantic Entropy** — A measure of uncertainty over the meaning of model outputs, as opposed to the specific tokens. Introduced by Kuhn et al. (ICLR 2023). PICE's VEC algorithm clusters evaluator outputs by semantic meaning and computes entropy over the clusters — halting when entropy converges.

**SLO (Service Level Objective)** — A target value or range of values for a service level measured by a service level indicator (SLI). Example: "99.9% of requests complete within 200ms." PICE extends the concept to verification: setting SLO-like targets for confidence levels, then using adaptive algorithms to meet them cost-efficiently.

**SPRT (Sequential Probability Ratio Test)** — A hypothesis testing procedure developed by Abraham Wald (1947) that examines data sequentially and makes a decision as soon as sufficient evidence accumulates. Proven by the Wald-Wolfowitz theorem to minimize expected sample size among all tests with equivalent error rates. PICE's core stopping rule.

**Type I Error (α, False Positive)** — Incorrectly rejecting correct code (flagging good code as bad). PICE's SPRT acceptance threshold is set to control this rate.

**Type II Error (β, False Negative)** — Incorrectly accepting defective code (passing bad code as good). PICE's SPRT rejection threshold is set to control this rate.

-----

## Software Engineering

**ADR (Architecture Decision Record)** — A document capturing an important architectural decision, its context, and consequences. Referenced in the PubNub pipeline example where the `architect-review` agent produces ADRs.

**AMBA AXI** — Advanced Microcontroller Bus Architecture, Advanced eXtensible Interface. An ARM standard for on-chip communication in SoC designs. Referenced as an analogy: AXI bus verification uses 44 protocol rules as executable assertions at every interface boundary — the hardware equivalent of what PICE builds for software seams.

**Cascading Failure** — A failure in one component that triggers failures in dependent components, potentially bringing down an entire system. Example: the AWS US-EAST-1 outage where a DNS race condition in DynamoDB cascaded across EC2, Lambda, and NLB for 14+ hours.

**CI/CD (Continuous Integration / Continuous Deployment)** — The practice of frequently integrating code changes and automatically deploying them. PICE's Stack Loops and seam checks can integrate into CI/CD pipelines.

**Cold Start** — The delay that occurs when a serverless function or container is invoked for the first time and needs to initialize. A common seam failure: code works after warm-up but fails under cold start timing constraints.

**Consumer-Driven Contract** — A testing pattern where the consumer of an API defines the contract (what it expects), and the provider verifies it satisfies that contract. Used by Pact. PICE extends this concept with automated assumption mining.

**Deutsch's Eight Fallacies** — Eight false assumptions programmers new to distributed applications invariably make (Peter Deutsch, 1994): the network is reliable, latency is zero, bandwidth is infinite, the network is secure, topology doesn't change, there is one administrator, transport cost is zero, the network is homogeneous. Still validated today as root causes of integration failures.

**DO-178C** — The FAA's software certification standard for airborne systems. Requires bidirectional traceability between all certification artifacts. Referenced as an analogy for comprehensive seam verification.

**DORA Metrics** — DevOps Research and Assessment metrics: deployment frequency, lead time for changes, change failure rate, time to restore service. Referenced as inspiration for PICE's self-evolving metrics.

**FPR (False Positive Rate)** — The proportion of negative results incorrectly identified as positive. In PICE: how often a check flags correct code as defective. A key metric in the self-evolving loop — high FPR checks get deprioritized.

**JSON-Lines** — A text format where each line is a valid JSON object, separated by newlines. Claude Code's streaming output format. PICE parses this when communicating with the CLI subprocess.

**JSON-RPC** — A remote procedure call protocol encoded in JSON. PICE's internal provider architecture uses JSON-RPC 2.0 over stdio for communication between the Rust core and TypeScript providers.

**MAPE-K** — Monitor → Analyze → Plan → Execute, over a shared Knowledge base. IBM's reference architecture for self-adaptive systems (Kephart & Chess, 2003). The architectural skeleton for PICE's self-evolving verification loop.

**Pact** — The de facto open-source consumer-driven contract testing framework (created 2013). Tests concrete request/response pairs from consumer tests against the actual provider. Verifies structural compatibility but not behavioral correctness.

**Schema Drift** — When the actual data structure at a service boundary diverges from the documented or expected schema over time. A common seam failure that PICE's seam checks target.

**Specmatic** — An API contract testing tool (formerly Qontract) that uses OpenAPI/AsyncAPI/gRPC specs as executable contracts directly, with backward compatibility checking via git-based spec comparison.

**SWE-Bench** — A benchmark for evaluating AI coding agents on real-world GitHub issues. SWE-Bench Verified tests single-file changes. SWE-Bench Pro tests multi-file changes (averaging 4.1 files, 107.4 lines). The performance drop from Verified to Pro (~70% → ~23%) demonstrates the seam problem.

**TLA+** — A formal specification language for designing and verifying concurrent and distributed systems, created by Leslie Lamport. Used by Amazon since 2011 across 10+ production systems (DynamoDB, S3, EBS), finding bugs requiring 35-step state traces.

**VIP (Verification IP)** — In hardware design, pre-built reusable protocol verification libraries that encode all protocol rules as executable assertions. Provided by Synopsys, Cadence, and others. Referenced as the hardware analog to what PICE builds for software integration boundaries.

**WAL (Write-Ahead Logging)** — A database journaling technique where changes are written to a log before being applied. SQLite with WAL mode enables concurrent reads during writes — recommended for PICE's metrics engine.

-----

## Benchmarks and Studies Referenced

**AlphaCode / AlphaCode 2** — DeepMind's code generation systems. AlphaCode generated up to 1 million samples per problem. AlphaCode 2 achieved equivalent performance with 10,000× fewer samples using better models and selection — demonstrating that algorithm quality dominates brute-force scaling.

**CodeRabbit Study** — Analysis of 470 PRs finding AI-generated code contains 1.7x more issues, with logic errors 1.75x more common and security vulnerabilities 1.5–2x higher.

**ConSol** — (Lee et al., March 2025) Applied SPRT to single-model self-consistency for LLM reasoning tasks. The closest prior art to PICE's Bayesian-SPRT, but applied only to single-model self-consistency, not heterogeneous multi-model code evaluation.

**DafnyComp** — Harvard benchmark for compositional specification in LLMs. Key finding: "LLMs handle local specs but fail under composition."

**GitClear Study** — Analysis of 211 million lines showing code duplication rose 8x in 2024 vs. pre-AI baseline, while refactoring collapsed from 24% to below 10%.

**Gregor et al. (ICST 2025)** — TU Munich/Siemens study producing a 23-category taxonomy of integration-relevant faults for microservice testing. 21 of 23 categories experienced by >50% of practitioners.

**Kim et al. (ICML 2025)** — "Correlated Errors in Large Language Models." Demonstrated across 350+ LLMs that models agree on ~60% of their errors. Foundation for PICE's correlated evaluator ceiling analysis.

**Large Language Monkeys (Brown et al., 2024)** — Stanford study showing SWE-bench solve rates climbing from 15.9% (1 sample) to 56% (250 samples) following an exponentiated power law, but with selection/verification as the bottleneck.

**SWE-Bench Pro (Scale AI, September 2025)** — Benchmark testing AI agents on 1,865 multi-file problems. Models scoring >70% on SWE-Bench Verified achieved only ~23% on Pro.

**SWE-CI (March 2026)** — Benchmark testing agents on CI maintenance. Zero-regression rate below 0.25 for most models — agents broke existing behavior in 75%+ of maintenance iterations.

**Weaver (Stanford/UW-Madison/Together AI, 2025)** — System closing the generation-verification gap using ensemble verification with learned verifier reliabilities. 30+ weak verifiers with probabilistic aggregation achieve 91% collective accuracy despite individual accuracy of only 43–62%.

-----

## Organizations and Tools Referenced

**Anthropic** — The company behind Claude and Claude Code. PICE integrates with Claude Code as an optional execution substrate.

**ArchUnit** — An open-source Java library for checking architecture constraints as unit tests. Checks static dependency rules but misses runtime coupling.

**Daikon** — A dynamic invariant detection tool from the University of Washington. Infers likely program invariants (e.g., "x > 0", "array is sorted") by observing execution. Mature (20+ years) but limited to single-component analysis.

**Develocity** — (Formerly Gradle Enterprise.) Build and test analytics platform. Netflix reported 280K developer hours saved/year through predictive test selection.

**Digma** — IDE plugin providing "preemptive observability" using OpenTelemetry instrumentation to surface runtime code insights without code changes.

**EvoSuite** — A tool using genetic algorithms to automatically generate test suites optimizing for code coverage. EvoSuiteFIT extends it with RL-based adaptive fitness function selection.

**Honeycomb** — An observability platform co-founded by Charity Majors, championing Observability-Driven Development. Distinguishes Observability 1.0 (separate metrics/logs/traces) from 2.0 (unified wide events in columnar storage).

**jQAssistant** — A tool storing code structural data in Neo4j for graph-based architecture analysis. Powerful but requires manual rule authoring in Cypher.

**Launchable** — (Now CloudBees Smart Tests.) ML-powered predictive test selection tool. Demonstrated 90% confidence from running only 20% of tests.

**OpenTelemetry** — An open-source observability framework for collecting traces, metrics, and logs. The observation infrastructure PICE v0.4 would integrate with for implicit contract inference at service boundaries.

**RESTler** — Microsoft Research's stateful REST API fuzzer. Analyzes OpenAPI specs to infer producer-consumer dependencies, then fuzzes multi-step sequences. Found 28 bugs in GitLab.

**Signadot SmartTests** — Infers API contracts by observing real service interactions rather than requiring manual definition. Used by DoorDash. The closest commercial tool to PICE v0.4's implicit contract inference, but doesn't perform bidirectional assumption comparison.

**Tracetest** — Creates assertions against distributed traces, turning production issues into automated test assertions in CI/CD.

-----

*Last updated: April 2026. Terms are added as new research is incorporated into the PICE roadmap.*
