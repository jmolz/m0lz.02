# Stack Loops and Arch Experts: Originality Analysis

*PICE Research Library — Expanded research supporting the [PICE Roadmap](../roadmap.md)*

*For term definitions, see the [Glossary](../glossary.md).*

-----

## Executive Summary

Neither "Stack Loops" nor "Arch Experts" exists as a pre-existing named concept, pattern, or framework anywhere in the surveyed landscape of software engineering, multi-agent AI systems, or AI-assisted development. After systematic searches across academic papers, framework documentation, developer blogs, and community discussions for all major platforms — CrewAI, AutoGen, LangGraph, MetaGPT, Claude Code, Cursor, and Windsurf — zero instances of either term used as a formal named concept were found. Both terms are **novel coinages by the PICE framework author**, though the underlying ideas they describe have significant conceptual overlap with existing work published under different names.

-----

## 1. Methodology

### Search strategy

Each term was searched across multiple domains using exact-match and semantic queries:

- **Academic databases:** arXiv, ACM Digital Library, IEEE Xplore, Google Scholar, Semantic Scholar
- **Framework documentation:** CrewAI, AutoGen/AG2, LangGraph/LangChain, MetaGPT, Claude Code, Cursor, Windsurf
- **Package registries:** npm, PyPI, crates.io
- **Developer communities:** GitHub (repos, issues, discussions), Reddit (r/programming, r/MachineLearning, r/ClaudeAI), Hacker News, Stack Overflow, Dev.to
- **Blog platforms:** Medium, Substack, personal engineering blogs
- **Patent databases:** USPTO, Google Patents

### Exclusion criteria

Results were excluded when:
- "Stack" and "loops" appeared as separate, unrelated concepts (e.g., "looping through a stack data structure")
- "Architecture expert" referred to a human job title rather than an AI agent pattern
- "Arch" referred to Arch Linux rather than software architecture

-----

## 2. "Stack Loops" — No Prior Art as a Named Pattern

### Search results

Searches across every relevant domain — DevOps, CI/CD, AI coding workflows, multi-agent orchestration, and software testing — returned **zero uses of "Stack Loops" as a proper named concept**. Every result containing both words was an incidental combination: "looping through a stack," "feedback loops in the DevOps stack," "stack overflow in infinite loops," or similar phrases where neither word connects to the other as a compound concept.

### Nearest existing terms

**"Loop Stack" (inverted word order)** — Coined by Duncan Krebs at KrebsNet as a recursive orchestration pattern for multi-agent AI. A product called Loopstack AI also exists as a TypeScript/YAML workflow framework. Neither describes what PICE's "Stack Loops" means — per-layer verification loops across a technology stack.

**Claude Code "Loops" feature** — Introduced in March 2026 via the `/loop` command for autonomous recurring tasks. Unrelated to per-layer stack verification. The term "loops" in the Claude Code context refers to iterative task execution, not layered architecture verification.

### Conceptual predecessors under different names

The individual building blocks behind Stack Loops are well-established. What's novel is their specific formulation and combination:

**Test Pyramid (Mike Cohn, 2009)** — The foundational concept of layered testing: unit tests at the base, integration in the middle, end-to-end at the top. Established that verification should be structured by layer, with different investment at each level. However, the Test Pyramid is a static model — it describes proportions, not iterative cycles. Stack Loops add the "loop" dimension: each layer runs its own Plan→Implement→Contract-Evaluate cycle independently.

**Speed Hierarchy of Feedback (Dark Software Fabric, January 2025)** — Defines a 7-layer verification hierarchy for AI-native development: types, lint rules, contract tests, unit tests, coverage analysis, AI logic checking, and end-to-end tests. Each layer runs at a different speed. This is arguably the closest published parallel to Stack Loops — it structures verification as ordered layers with iterative feedback. However, it focuses on test type ordering rather than technology stack layers (backend, database, infrastructure, deployment), and doesn't include the "always-run layer" concept or seam verification between layers.

**Verification Loops (Spotify Engineering, December 2025)** — Describes inner/outer feedback loops for AI coding agents: the inner loop is fast iteration within the IDE; the outer loop is CI/CD validation. Related to the "loop" concept but not structured by stack layers — these are developer workflow loops, not architecture verification loops.

**Quality Filtration Stacks (Capital One)** — Uses a water-filter metaphor for layered defect catching, where each filter removes a different class of defect. Conceptually adjacent — defects pass through ordered layers of verification. But the "stack" is a filter metaphor, not a technology stack, and there's no iterative loop per layer.

**StackPlanner (arXiv:2601.05890, January 2025)** — A centralized hierarchical multi-agent system with task-experience memory management. Uses "stack" in the name but refers to task decomposition hierarchies, not technology stack layers.

### What makes Stack Loops novel

The specific combination that no prior work captures:

1. **Technology stack layers** (backend, database, API, infrastructure, deployment) rather than test type layers (unit, integration, E2E)
2. **Independent PICE loops per layer** — each layer runs its own Plan→Implement→Contract-Evaluate cycle
3. **Always-run layers** — infrastructure, deployment, and observability cannot be skipped regardless of change scope
4. **Seam verification between layers** — checking integration contracts at layer boundaries, not just within layers
5. **Dependency ordering** — layers run sequentially based on architectural dependencies
6. **Adaptive pass count** — each layer's evaluation depth is determined by the ADTS/Bayesian-SPRT/VEC algorithms based on accumulated evidence

No single prior work addresses more than two of these six properties simultaneously.

-----

## 3. "Arch Experts" — No Prior Art as a Named Pattern

### Search results

The term "Arch Experts" — meaning dynamically generated specialist agents based on project architecture discovery — does not appear in any surveyed source as a formal concept. The only exact-match result was **Codementor.io/arch-experts**, which lists Arch Linux freelance developers — entirely unrelated.

### Nearest existing terms and systems

**ArchE — Architecture Expert (CMU SEI, 2003–2008)** — A rule-based Eclipse plugin from Carnegie Mellon's Software Engineering Institute. Helped human architects make quality-attribute-driven design decisions using the JESS expert system engine. Fundamentally different: a traditional expert system tool for human use, not a pattern for dynamically generating AI agents from architecture discovery. Discontinued.

**ArchAgent (arXiv:2602.22425, February 2026)** — Uses "Arch" in its name but describes hardware architecture discovery — specifically cache replacement policies via AlphaEvolve. Operates in the computer architecture domain (chip design), not software project architecture.

**AutoAgents (Chen et al., arXiv:2309.17288, September 2023)** — A framework that dynamically synthesizes specialized expert agents based on task content rather than using predefined roles. The closest match to the "dynamic generation" aspect of Arch Experts. Key difference: AutoAgents generates agents from task descriptions ("build a web scraper"), while Arch Experts generates agents from project architecture files (package.json, Dockerfile, docker-compose.yml). The input signal is fundamentally different — task intent vs. existing infrastructure reality.

**MetaGPT (2023)** — Includes a dedicated "Architect" agent role within its software-company simulation. This is a fixed, predefined role, not dynamically generated. Every MetaGPT project gets the same Architect regardless of the technology stack.

**Codified Context domain-expert agents (Vasilopoulos, arXiv:2602.20478, February 2026)** — Describes **19 specialized domain-expert agents** with trigger tables for automatic task routing in a 108,000-line C# codebase. Functionally very similar to Arch Experts in practice, but with critical differences:
- Agents are **manually authored** by the development team, not dynamically generated
- Trigger tables are **hand-crafted** routing rules, not architecture-inferred
- The system requires explicit configuration for each new agent
- Limited to a single codebase; not generalizable across projects

**vFunction** — Describes making AI agents "architecture-aware co-pilots" by feeding discovered architectural context into coding agents. Related concept — using architecture discovery to enhance AI behavior — but the agents themselves are not generated from the discovery. The architecture context is an input to a generic agent, not a generator of specialist agents.

**Archyl** — Offers automated C4 model generation from code with MCP integration for agent queries. Generates architecture documentation, not specialist agents.

### Framework documentation sweep

A systematic check of all major multi-agent frameworks confirmed neither term appears:

| Framework | Agent specialization approach | Uses "Stack Loops"? | Uses "Arch Experts"? |
|---|---|---|---|
| CrewAI | Role-based with defined roles, goals, backstories | No | No |
| AutoGen/AG2 | Dynamic group chat, expert tools | No | No |
| LangGraph | Four patterns: subagents, skills, handoffs, routers | No | No |
| MetaGPT | Fixed roles: PM, Architect, Engineer, QA | No | No |
| Claude Code | Custom agents via markdown files | No | No |
| Cursor | Rules files for context | No | No |
| Windsurf | Cascade workflows | No | No |

### What makes Arch Experts novel

The specific combination that no prior work captures:

1. **Architecture-inferred, not configured** — experts emerge from scanning project files, not from manual agent definition
2. **Technology-specific system prompts** — each expert's instructions are constructed from the actual configuration files it will evaluate, not from a generic template
3. **Seam ownership** — each expert owns the boundaries around its component, declaring what it provides and what it assumes
4. **No template library** — the system doesn't select from a pre-built catalog; it constructs experts de novo for each project's specific technology combination
5. **Adversarial assumption mining** — the dual-model evaluation is repurposed for seam discovery, with each model independently inferring one side of the integration contract
6. **Runtime AgentDefinition construction** — experts are ephemeral objects passed via CLI flags, not persisted configuration files

No existing system combines architecture discovery with dynamic agent generation with seam ownership with adversarial assumption mining.

-----

## 4. Additional Novel Concepts in PICE

Beyond Stack Loops and Arch Experts, PICE introduces several other concepts that appear to be original:

### Seam Verification

The specific practice of running verification checks at the boundaries between architectural layers, using the twelve empirically validated failure categories as a checklist. While integration testing and contract testing exist, the concept of structured seam-specific verification mapped to a failure taxonomy — and integrated into a per-layer loop system — is novel in this formulation.

### Adversarial Assumption Mining

Using dual-model adversarial evaluation specifically to discover implicit contract asymmetries — one model infers consumer assumptions, the other infers provider guarantees, and the framework flags mismatches. The concept of adversarial LLM debate exists (Du et al., 2024), but its application to seam-level assumption discovery is new.

### Implicit Contract Inference (v0.4)

The synthesis of Daikon-style invariant detection + spec mining + distributed tracing + session types + hardware VIP + chaos engineering, applied to infer behavioral contracts at service boundaries. Each individual research lineage is mature; the combination is unpublished.

### Bayesian-SPRT Adaptive Halting, ADTS, and VEC (v0.2 algorithms)

Three algorithms for adaptive evaluation depth. ConSol (March 2025) applied SPRT to single-model self-consistency. PICE's Bayesian-SPRT extends this to heterogeneous multi-model evaluation with Bayesian priors. ADTS and VEC have no direct precedent in AI code verification.

### Self-Evolving Verification (v0.5)

The combination of MAPE-K + predictive test selection + DSPy-style prompt optimization + ensemble reliability weighting + evolutionary check generation in a single closed-loop verification framework. Each pattern exists independently; the integration is novel.

-----

## 5. Conceptual Predecessor Map

The table below maps each PICE concept to its closest known parallels, highlighting what's borrowed and what's new:

| PICE Concept | Closest Existing Work | What's Borrowed | What's New |
|---|---|---|---|
| Stack Loops | Speed Hierarchy of Feedback (2025) | Layered verification ordering | Technology stack layers, independent PICE loops per layer, always-run layers, seam checks |
| Stack Loops | Test Pyramid (2009) | Layered testing concept | Iterative loop mechanics, adaptive pass count, dependency ordering |
| Stack Loops | Verification Loops (Spotify, 2025) | Feedback loops for AI agents | Per-layer structure, seam verification, tier-scaled depth |
| Arch Experts | AutoAgents (2023) | Dynamic expert agent generation | Architecture-file inference (vs. task-content inference), seam ownership |
| Arch Experts | Codified Context (2026) | Domain-expert agents with routing | Dynamic generation (vs. manual authoring), no template library |
| Arch Experts | MetaGPT Architect (2023) | Dedicated architecture role | Dynamic per-project generation (vs. fixed role) |
| Seam Verification | Pact contract testing (2013) | Boundary verification concept | Automated from failure taxonomy, behavioral not just structural |
| Seam Verification | Hardware VIP (industry) | Protocol assertions at interfaces | Applied to software (first time), integrated with AI evaluation |
| Assumption Mining | Adversarial LLM debate (2024) | Dual-model disagreement as signal | Applied to implicit contract discovery at seams (first time) |
| Implicit Contracts | Daikon invariant detection (2001) | Behavioral property inference | Cross-service (vs. single-component), distributed traces |
| Adaptive Algorithms | ConSol SPRT (2025) | Sequential stopping for LLMs | Multi-model with Bayesian priors, ADTS divergence routing, VEC entropy |
| Self-Evolving | Meta PTS (2019) + DSPy (2024) | ML-driven optimization, prompt tuning | Combined into single verification framework with evolutionary generation |

-----

## 6. Conclusion

Both **Stack Loops** and **Arch Experts** are original coined terms with no pre-existing usage as named concepts in software engineering, multi-agent AI, or AI-assisted development. The underlying ideas — layered verification across technology stacks and architecture-aware specialist agents — have substantial conceptual precedent under different names, particularly in the 2023–2026 explosion of multi-agent AI research.

However, the specific formulations, the compound terminology, and especially the **combination of both concepts within a unified framework** — enhanced with seam verification, adversarial assumption mining, adaptive convergence algorithms, and self-evolving verification — represent genuinely novel contributions.

The novelty is not in the individual building blocks (which are well-established across multiple fields) but in:
1. The specific formulation of each concept
2. The combination into a unified architecture
3. The mathematical grounding (convergence analysis, correlated evaluator theory)
4. The cross-domain synthesis (hardware VIP, clinical trial stopping rules, psychometric testing, control theory)
5. The closed-loop self-evolution from collected execution data

This is the pattern of real innovation: synthesizing mature ideas from multiple fields into a combination that no one has attempted, creating something that is more than the sum of its parts.

-----

*See also: [Seam Blindspot](seam-blindspot.md) | [Convergence Analysis](convergence-analysis.md) | [Claude Code Integration](claude-code-integration.md) | [Glossary](../glossary.md)*
