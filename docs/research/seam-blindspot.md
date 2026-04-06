# The Seam Blindspot: Where Software Really Breaks and What No One Is Building to Fix It

*PICE Research Library — Expanded research supporting the [PICE Roadmap](../roadmap.md)*

*For term definitions, see the [Glossary](../glossary.md).*

-----

## Executive Summary

Software systems fail overwhelmingly at the boundaries between components — not inside them. Google's analysis of thousands of postmortems reveals that 68% of all outages are triggered by configuration and binary pushes at integration points. AI coding agents make this worse: SWE-Bench Pro shows models achieving only ~23% on multi-file tasks vs. >70% on single-file tasks. Yet the entire verification tooling landscape — from contract testing to architecture analysis to formal methods — systematically underserves these boundaries. The most dangerous failures occur where Component A assumes something about Component B that Component B never explicitly guarantees, and no tool in existence can automatically detect this asymmetry.

This report synthesizes research across seven domains to identify what would make PICE's seam verification genuinely novel and differentiated.

-----

## 1. A Rigorous Taxonomy of Integration Failures

### The twelve empirically validated failure categories

Industry postmortem databases converge on twelve patterns that recur across every major infrastructure:

**1. Configuration/deployment mismatches.** Google SRE data (2010–2017): 31% of outage triggers. 82% of configuration-related triggers stem from manual oversight at boundaries. Configuration changes propagate through integration points where assumptions about environment variables, feature flags, and deployment parameters diverge between producer and consumer.

**2. Binary/version incompatibilities.** Google SRE: 37% of outage triggers. Version skew between services that share interfaces — a producer upgrades its serialization format, but the consumer still expects the old format. This is technically a schema drift issue but manifests as a version management problem.

**3. Protocol/API contract violations.** Adyen's study of 2.43 million API error responses (ICSE-SEIP 2018) identified 11 general causes, dominated by invalid/missing request data and third-party integration failures. Over 60,000 daily errors from integration faults alone at a single large payment company.

**4. Authentication handoff failures.** Empirical studies of microservice systems show 4.55% of all issues relate to authentication and authorization handoffs between services — tokens not propagated, credential formats mismatched, session state not shared correctly across service boundaries.

**5. Cascading failures from dependency chains.** AWS US-EAST-1 (October 2025): a DNS race condition in DynamoDB's management system cascaded across EC2, Lambda, and NLB for 14+ hours. A single seam failure propagating through an entire architecture. Netflix's experience with cascading failures led to the development of Hystrix and resilience patterns specifically targeting integration boundaries.

**6. Retry storm / timeout policy conflicts.** When Service A's retry count multiplied by Service B's timeout exceeds Service B's capacity, the retries themselves become the outage. Documented as a primary failure mode at Netflix, Amazon, and Uber. Michael Nygard (Release It!) calls this "the integration point amplifier."

**7. Service discovery failures.** Validated by >50% of practitioners in Gregor et al.'s survey (ICST 2025, TU Munich/Siemens). Services fail to locate each other due to stale DNS, misconfigured load balancers, or service registry inconsistencies — particularly during deployments when old and new instances coexist.

**8. Health check blind spots.** AWS US-EAST-1 (December 2021): the monitoring system itself failed to failover, masking the outage from operators. Health checks that don't account for dependency health, cold start timing, or partial functionality create false confidence at integration boundaries.

**9. Serialization/schema drift.** When the actual data structure at a service boundary diverges from the documented or expected schema over time. Optional fields that become required in practice, nullable fields that are never actually null, enum values that expand without consumer awareness.

**10. Cold start and ordering dependencies.** Service A assumes Service B is already running. In serverless architectures, cold start latency can push response times past timeout thresholds that work fine after warm-up. In container orchestration, startup ordering is often implicit rather than enforced.

**11. Network topology assumptions.** Deutsch's Eight Fallacies of Distributed Computing (1994) remain validated three decades later: the network is reliable, latency is zero, bandwidth is infinite, the network is secure, topology doesn't change, there is one administrator, transport cost is zero, the network is homogeneous. Every fallacy is an assumption about a seam.

**12. Resource exhaustion at boundaries.** Thread pools, connection pools, and file descriptor limits consumed by slow or hung integration calls. A single slow downstream service can exhaust the connection pool of every upstream caller, turning a performance issue into a complete outage.

### The 23-category academic taxonomy

Gregor et al.'s comprehensive taxonomy (ICST 2025) organizes faults around service lifecycle phases:

- **Service Description Faults** — Incorrect or incomplete API specifications, missing documentation of side effects, undocumented error codes.
- **Deployment Faults** — Configuration mismatches between environments, missing dependencies, incorrect resource allocation.
- **Discovery Faults** — Service registry inconsistencies, stale DNS, incorrect load balancer configuration.
- **Composition Faults** — Incorrect service choreography, missing compensating transactions, incomplete saga implementations.
- **Binding Faults** — Protocol mismatches, authentication failures, TLS/SSL configuration errors.
- **Execution Faults** — Timeout violations, retry storms, cascading failures, data consistency violations.

21 of 23 fault categories were experienced by over 50% of surveyed practitioners — confirming these are systemic, not edge cases.

### The cost is staggering

Gartner recognizes "integration technical debt" as a distinct category, finding it leads to poor adaptability and higher costs. The "interoperability tax" is estimated to consume up to 40% of IT budgets across enterprises, with healthcare alone spending $30 billion annually just making systems communicate.

-----

## 2. Contract Verification: Structure vs. Behavior

### The current tooling landscape

**Pact** (created 2013, open source) — The de facto consumer-driven contract testing tool. Generates concrete request/response pairs from consumer tests and verifies them against the actual provider. Supports HTTP, message queues, and GraphQL. Used by companies from startups to enterprises. Limitation: tests structural compatibility (data shape matches), not behavioral correctness (ordering, timing, state transitions).

**Specmatic** (formerly Qontract) — Uses OpenAPI/AsyncAPI/gRPC specifications as executable contracts directly. Performs backward compatibility checking via git-based spec comparison. Ships an MCP server for Claude Code integration. Limitation: limited to spec-defined contracts; can't discover implicit behavioral contracts.

**Spring Cloud Contract** — JVM-ecosystem contract testing. Producer writes contracts in Groovy/YAML; the framework generates tests for both sides. Tightly integrated with Spring Boot. Limitation: JVM-only; manual contract authoring.

**Schemathesis** — Property-based testing for APIs. Automatically generates thousands of test cases from OpenAPI schemas, including edge cases the developer wouldn't think to write. Used by Spotify, JetBrains, Red Hat. Limitation: tests schema compliance, not behavioral invariants.

**Buf** — The leading tool for Protocol Buffer schema management. 53 breaking change rules. Schema registry preventing unintended breaking changes. Adopted by CockroachDB and Netflix. Limitation: protobuf only; structural, not behavioral.

**Session Types** (Honda, Yoshida, Carbone — POPL 2008) — Mathematical framework guaranteeing communication safety, deadlock-freedom, and protocol fidelity. Implementations exist in Rust (mpst-rust, used for Amazon Prime Video protocols), Python, Scala, and TypeScript via the Scribble protocol description language from Imperial College. Limitation: zero adoption in production microservices. Requires protocol formalization that doesn't match how services are actually built.

### The critical gap

| Capability | Pact | Specmatic | Schemathesis | Buf | Session Types |
|---|---|---|---|---|---|
| Structural schema validation | ✅ | ✅ | ✅ | ✅ | ✅ |
| Breaking change detection | ❌ | ✅ | ❌ | ✅ | ❌ |
| Protocol ordering verification | ❌ | ❌ | Partial | ❌ | ✅ |
| Behavioral/semantic verification | ❌ | ❌ | ❌ | ❌ | Partial |
| Cross-service invariant checking | ❌ | ❌ | ❌ | ❌ | ❌ |
| Failure mode contracts | ❌ | ❌ | ❌ | ❌ | ❌ |
| Implicit contract inference | ❌ | ❌ | ❌ | ❌ | ❌ |

Every practical tool verifies structural compatibility — that the shape of data matches. None verifies behavioral correctness — that components actually agree on ordering, timing, state transitions, error handling, or capacity. This is the bridge that doesn't exist between session-type theory and industry practice.

The most promising emerging work is **Signadot SmartTests**, which infers API contracts by observing real service interactions rather than requiring manual definition — used by DoorDash to cut integration feedback from 30+ minutes to under 2 minutes. But even Signadot doesn't perform bidirectional assumption comparison.

-----

## 3. AI Agents Are Systematically Blind at Integration Boundaries

### The empirical evidence

**SWE-Bench Pro** (Scale AI, September 2025) tested top models on 1,865 problems requiring patches across multiple files averaging 4.1 files and 107.4 lines. Performance collapsed: models scoring >70% on SWE-Bench Verified achieved only ~23% on SWE-Bench Pro. The failure mode analysis: larger models primarily fail on "semantic or algorithmic correctness in large, multi-file edits" — precisely the seam problem.

**SWE-CI** (March 2026) tested agents on continuous integration maintenance across 233 days of repository evolution. The most common failure pattern was cascading regression: fix one test → break another module → patch that → break something else. The zero-regression rate was below 0.25 for most models. The documented mechanism: "function signatures change but callers are not updated across the codebase."

**CodeRabbit's analysis** of 33,596 agent-authored PRs found unmerged PRs "tend to involve larger, more invasive code changes, touch more files, and often do not pass CI/CD pipeline validation." Analysis of 470 PRs: AI-generated code contains 1.7x more issues, with logic/correctness errors 1.75x more common, business logic errors >2x, and security vulnerabilities 1.5–2x higher.

**GitClear** (211 million lines): code duplication rose 8x in 2024 vs. pre-AI baseline, refactoring collapsed from 24% to below 10%.

### Ten systematic failure modes

1. **Ripple effect blindness** — AI changes one component without updating all dependents. Function signatures change but callers aren't updated; data models evolve but serialization stays stale.
2. **Context window limitations** — Even with 200K+ tokens, agents can't hold entire system architectures in context. Critical integration information (deployment topology, service discovery, auth flows) often isn't in the code at all.
3. **Happy path bias** — 43% of patches fix the primary issue but introduce new failures under adverse conditions.
4. **Code island effect** — AI generates isolated additions rather than integrating with existing code, creating disconnected islands that work individually but don't compose.
5. **Convention blindness** — AI doesn't internalize implicit project norms, generating "generic defaults" that drift from repository-specific patterns.
6. **Infrastructure ignorance** — AI generates application code but systematically misses Docker networking, environment variables, service discovery, and CI/CD pipeline requirements.
7. **Error handling gaps** — AI implements the success path but leaves error boundaries undefined, creating implicit contracts about failure behavior.
8. **Concurrency blindness** — AI generates code that works under sequential execution but fails under concurrent access at service boundaries.
9. **State management fragmentation** — AI creates local state management that doesn't synchronize with the broader system's state model.
10. **Test isolation bias** — AI writes tests that pass in isolation but don't verify integration behavior.

### The root cause

Harvard's research on compositional specification (DafnyComp benchmark) identified it: "LLMs handle local specs but fail under composition." Models treat implementation and specification as independent generation tasks rather than coupled constraints. This is the fundamental reason AI agents fail at seams — they optimize for local correctness, not global structural integrity.

-----

## 4. Architecture Analysis Tools: Structure Without Behavior

### Current tools

**ArchUnit** (open-source, Java) — Checks package structure violations and layering constraints as JUnit tests. Limited to Java bytecode; misses anything not in static type relationships.

**jQAssistant** — Stores structural data in Neo4j for graph-based architecture queries. Powerful but requires manual rule authoring in Cypher.

**Structure101** (acquired by Sonar, October 2024) — Excels at cyclic dependency detection. Being folded into SonarQube Cloud.

**Designite** — Detects 7 architecture smells, 19 design smells, 11 implementation smells. Limited to C# and Java.

**Arcan** (University of Milano-Bicocca) — Most research-advanced tool, extending to microservice smells including cyclic dependencies, hard-coded endpoints, and shared persistence.

**Drift** (2025, open-source Python) — Specifically targets architectural erosion from AI-generated code, measuring structural entropy via a "drift score."

**vFunction** — Combines static and dynamic analysis with AI to map application domains and dependencies at runtime.

### What they miss

| Failure cause | Static tools detect? | Dynamic tools detect? |
|---|---|---|
| Forbidden/cyclic dependencies | ✅ | N/A |
| Shared database coupling | ❌ | Partial |
| Temporal coupling | ❌ | Partial |
| Semantic duplication across services | ❌ | ❌ |
| Retry/cascade patterns | ❌ | Partial |
| Config drift between environments | ❌ | ❌ |
| Feature flag coupling | ❌ | ❌ |
| Cross-cutting concern inconsistency | ❌ | ❌ |
| AI-accelerated architectural drift | Emerging | ❌ |

WunderGraph's analysis captures it: "There is no widely adopted solution that makes all types of microservice dependencies explicit and manageable at design time."

-----

## 5. Cross-Domain Verification Approaches

### Hardware verification: VIP modules

In chip design, the Verification IP (VIP) ecosystem provides pre-built, reusable protocol verification libraries for every major bus standard (AMBA AXI, PCIe, USB, etc.). Synopsys and Cadence sell VIP modules encoding all protocol rules as executable assertions running continuously at interface boundaries. A synthesizable AMBA AXI protocol checker encodes 44 rules for verifying on-chip communication. SystemVerilog's `interface` construct bundles signals, protocol assertions, bus functional models, and coverage metrics into a single reusable component — verification logic travels with the interface definition.

**Software has no equivalent.** There is no "OpenAPI VIP" or "gRPC VIP" that comprehensively validates every protocol rule at integration boundaries. PICE builds this: seam checks are protocol-specific verification modules that travel with layer boundary definitions.

### Distributed systems formal methods

Amazon has used **TLA+** since 2011 across 10+ production systems (DynamoDB, S3, EBS), finding bugs requiring state traces of 35 steps — impossible to find via testing. Microsoft's **P language** compiles state-machine programs into executable C/C# code, bridging the model-implementation gap. P's **PObserve** feature validates production service logs against formal specifications — runtime conformance checking against design-time models. **Stateright** (Rust) takes the most radical approach: the verified model IS the implementation, deployed as actual network actors after model-checking.

### Stateful API fuzzing

**RESTler** (Microsoft Research) is the first stateful REST API fuzzer. It analyzes OpenAPI specs to infer producer-consumer dependencies among request types, then fuzzes multi-step sequences exercising states only reachable through specific request chains. Found 28 bugs in GitLab and multiple bugs in Azure/Office365. This is hardware-style constrained-random verification applied to API boundaries.

### Safety-critical systems

**DO-178C** (aviation) requires bidirectional traceability between all certification artifacts — from requirements → architecture → code → tests → results. Every integration boundary has complete verification chain evidence. **AUTOSAR** (automotive) provides three interface types with formal checking. Researchers built A2A to automatically model AUTOSAR architectures as timed automata verified by the Uppaal model checker.

### Three transferable concepts

1. **Hardware VIP for software protocols** — Pre-built verification libraries per protocol (REST, gRPC, GraphQL, message queues) encoding all protocol rules as executable assertions, not just schema validation.
2. **PObserve-style runtime conformance** — Continuous validation of production traffic against formal specifications, checking protocol-level correctness rather than just latency and error rates.
3. **Supply chain attestation for interface contracts** — Extending Sigstore/SLSA to sign interface contract compliance attestations per build.

-----

## 6. Five Capabilities No One Has Built

After exhaustive research across all domains, five critical capabilities do not exist in any current tool, framework, or research prototype:

### Gap 1: Cross-component assumption asymmetry detection

No tool can automatically discover that Component A assumes X about Component B while Component B doesn't guarantee X. Daikon infers invariants within a single component. Pact tests explicitly written contracts. Garlan et al. identified this as "architectural mismatch" in 1995, but proposed documentation, not automated detection.

**PICE's approach:** Adversarial dual-model evaluation. One model infers consumer assumptions; the other infers provider guarantees. PICE flags asymmetries.

### Gap 2: Automated cross-service implicit contract inference

Daikon-style invariant detection has never been applied at service boundaries using distributed traces. No tool takes production traffic between two services and infers behavioral contracts: "this field is never null in practice," "responses always arrive within 150ms," "this endpoint is always called after that one."

**PICE's approach:** Analyze distributed traces at service boundaries, cluster behavioral patterns, and surface implicit contracts that no one declared.

### Gap 3: Seam drift detection

No tool establishes a behavioral baseline at an integration point and monitors for gradual divergence: response time distributions shifting, optional fields becoming always-present, ordering guarantees weakening.

**PICE's approach:** SLO monitoring for discovered (not declared) behavioral properties.

### Gap 4: Change impact analysis against implicit contracts

No tool evaluates a proposed code change against the corpus of inferred implicit contracts to predict which downstream assumptions it might violate.

**PICE's approach:** Pre-deployment gate that warns "this change moves p95 latency from 180ms to 250ms, and Service B assumes responses within 200ms."

### Gap 5: Adversarial integration test generation

No tool mines implicit assumptions from production behavior and generates targeted tests probing those specific assumptions — "Service A assumes this field is never null; here's a test that sends null."

**PICE's approach:** Targeted assumption validation, not random fuzzing.

-----

## 7. How This Maps to PICE

**Stack Loops** target the twelve failure categories at each technology layer — not just "does the code work" but "do the seams between layers hold." Each Stack Loop iteration includes a seam verification pass checking integration contracts with adjacent layers.

**Arch Experts** own the boundaries around their components, not just the components themselves. Each expert declares what its component provides and what it assumes. The adversarial evaluation model (dual-model) surfaces assumption asymmetries.

**Implicit Contract Inference** (v0.4) synthesizes the research lineages that have never been combined: Daikon + spec mining + distributed tracing + session types + hardware VIP + chaos engineering, orchestrated by AI.

**Self-Evolving Verification** (v0.5) tracks which seam checks catch real issues, which generate noise, and which failure categories are most common in each project — then uses that data to prioritize, tune, and evolve the verification strategy over time.

The differentiation is clear: while every other AI coding tool optimizes for generating correct code within components, PICE is the first to systematically verify that the spaces between components actually hold.

-----

*See also: [Convergence Analysis](convergence-analysis.md) | [Self-Evolving Verification](self-evolving-verification.md) | [Claude Code Integration](claude-code-integration.md) | [Glossary](../glossary.md)*
