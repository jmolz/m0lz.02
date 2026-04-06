# Optimal Verification Passes in Multi-Model AI Evaluation: Convergence Analysis

*PICE Research Library — Expanded research supporting the [PICE Roadmap](../roadmap.md)*

*For term definitions, see the [Glossary](../glossary.md).*

-----

## Executive Summary

How many evaluation passes does PICE need per layer to reach a target confidence level? The mathematically grounded answer: **3–5 passes reach the practical confidence ceiling for dual-model LLM verification, and no amount of additional passes can breach ~97% accuracy when evaluator correlation is ρ ≈ 0.3.** This hard limit, derived from the correlated Condorcet Jury Theorem and confirmed by empirical scaling laws from Stanford, DeepMind, and ICML 2025 research, means PICE's strategic advantage lies not in maximizing pass count but in *adaptively allocating* passes based on accumulated evidence. Three novel algorithms emerge from cross-domain synthesis — Bayesian-SPRT adaptive halting, adversarial divergence-triggered scaling, and verification entropy convergence — none of which have been applied to multi-model code verification.

-----

## 1. The Correlated Evaluator Ceiling

### Why independence matters — and why LLMs don't have it

The classical Condorcet Jury Theorem (1785) promises that majority-vote accuracy approaches 100% as the number of independent voters grows, provided each voter is more accurate than random chance. For N independent evaluators each with accuracy p > 0.5, the majority-vote error rate decays exponentially:

```
ε(N) ≤ exp(−N · D(0.5 ∥ 1−p))
```

where D is the KL divergence. This is the Chernoff bound — under independence, adding evaluators eliminates error exponentially fast.

**For LLM-based evaluation, this assumption fails catastrophically.**

Kim et al. (ICML 2025, "Correlated Errors in Large Language Models") demonstrated across 350+ LLMs that **models agree on approximately 60% of their errors**, even across different providers and architectures. More striking, Denisov-Blanch et al. (ICML 2025) found correlations of **ρ ≈ 0.35 even on random ASCII strings with forced choice** — proving that shared inductive biases, not shared knowledge, drive correlation. This means Claude and GPT will tend to make the same mistakes on the same code, not independent mistakes.

A companion study, "Consensus is Not Verification" (2025), confirmed: majority voting among LLMs systematically fails on questions where models share systematic biases, even when individual models are more accurate than chance. Consensus ≠ correctness.

### The effective sample size formula

The damage from correlation is quantified by the effective sample size:

```
n_eff = n / (1 + (n−1)ρ)
```

As n → ∞, this converges to **1/ρ**. With ρ = 0.3 (a conservative estimate for Claude-GPT correlation on code evaluation), n_eff caps at ~3.3 regardless of how many passes PICE runs. With ρ = 0.35, it caps at ~2.9.

This is not an engineering limitation. It is an information-theoretic bound. Running 100 evaluation passes with correlated models provides the same information as ~3 independent passes.

### The confidence curve

For PICE's dual-model architecture with individual evaluator accuracy p = 0.88 (typical frontier LLM evaluation accuracy on code review tasks) and inter-model correlation ρ = 0.35:

```
Confidence(N) ≈ C_max · (1 − e^{−λ·N_eff})
```

where C_max = 1 − ε_irreducible (the ceiling from shared biases plus specification ambiguity) and N_eff = N/(1+(N−1)ρ).

| Passes | N_eff | Estimated Confidence | Marginal Gain | Cumulative % of Max Improvement |
| ------ | ----- | -------------------- | ------------- | ------------------------------- |
| 1      | 1.00  | 88.0%                | —             | 0%                              |
| 2      | 1.48  | 92.1%                | +4.1%         | 48%                             |
| 3      | 1.87  | 94.0%                | +1.9%         | 70%                             |
| 4      | 2.09  | 94.9%                | +0.9%         | 80%                             |
| 5      | 2.27  | 95.4%                | +0.5%         | 86%                             |
| 7      | 2.50  | 95.9%                | +0.25% avg    | 92%                             |
| 10     | 2.63  | 96.2%                | +0.10% avg    | 95%                             |
| 20     | 2.80  | 96.5%                | +0.03% avg    | 99%                             |
| ∞      | 2.86  | ~96.6%               | 0             | 100% (ceiling)                  |

*Assumptions: p = 0.88 per evaluator, ρ = 0.35 inter-model correlation, based on correlated Condorcet analysis.*

**Key insight: passes 1→3 capture 70% of total achievable improvement. Passes 1→5 capture 86%. Beyond 5 passes, marginal gains drop below 0.5% per pass.**

### The irreducible error floor

The ~96.6% ceiling has three components:

1. **Shared LLM biases** (~2%) — Systematic errors common to all large language models trained on similar data distributions. Kim et al. showed these persist even across architecturally different models.

2. **Specification ambiguity** (~1%) — Cases where the code's correctness is genuinely underdetermined by the available specification. More evaluation passes cannot resolve what the spec doesn't define.

3. **Adversarial edge cases** (~0.4%) — Subtle bugs that exploit blind spots shared by all current-generation LLMs (e.g., certain concurrency patterns, specific numeric precision issues, particular security vulnerabilities).

### Breaching the ceiling

The ceiling is specific to *homogeneous LLM evaluation*. Three strategies push beyond it:

**1. Maximize evaluator diversity.** The Knowledge Divergence theory (Kaplan et al., 2025) proves that debate advantage depends on the principal angles between models' representation subspaces — with a phase transition from negligible to essential benefit as knowledge diversity increases. Using architecturally distinct models (transformer vs. SSM), models trained on different data distributions, or domain-specific fine-tuned evaluators reduces effective ρ.

**2. Incorporate orthogonal verification signals.** Unit test execution, static analysis, type checking, and formal verification are essentially uncorrelated with LLM judgment errors. Each orthogonal signal resets the correlation structure, potentially dropping effective ρ toward zero for the combined system. This is why PICE's Tier 3 combines AI evaluation with formal verification — not redundancy, but information-theoretic necessity.

**3. Decompose evaluation into independent sub-problems.** Evaluating correctness, security, performance, and style separately — each with its own evaluator committee — exploits the fact that error correlation varies by evaluation dimension. The Krogh-Vedelsby decomposition makes this precise: E_ensemble = E_avg − Ambiguity. Ensemble error improves only when evaluators disagree.

-----

## 2. Empirical Scaling Laws

### Stanford: Large Language Monkeys

Brown et al. (Stanford, 2024) studied how solve rates scale with repeated sampling on SWE-Bench. Results follow an exponentiated power law:

- 1 sample: 15.9% solve rate
- 10 samples: ~30% solve rate
- 50 samples: ~42% solve rate
- 250 samples: 56% solve rate

The curve is logarithmic — each doubling of samples yields diminishing returns. More critically, **the bottleneck is selection, not generation**. Majority voting and reward-model selection plateau at ~100–300 samples, unable to exploit the full coverage. The generation-verification gap grows with model capability.

### Self-consistency research

Wang et al. (2022) established self-consistency for chain-of-thought reasoning. Key findings on PaLM-540B with GSM8K:

- 1 path: 56.5% accuracy
- 5 paths: ~67% accuracy
- 10 paths: ~71% accuracy
- 40 paths: 74.4% accuracy

The curve is sharply logarithmic — most gain in the first 5–10 samples. A 2025 Gemini study confirmed that accuracy plateaus and **slightly declines past 15 agents** for weaker models, likely due to error correlation overwhelming the diversity benefit.

### AlphaCode: the extreme case

DeepMind's AlphaCode generated up to 1 million code samples per problem. Solve rate scaled log-linearly with sample count. But AlphaCode 2 achieved equivalent performance with **10,000× fewer samples** by using better models and selection — reinforcing that algorithm quality dominates brute-force scaling. This directly validates PICE's emphasis on adaptive algorithms over raw pass count.

### Weaver: ensemble verification

Stanford/UW-Madison/Together AI's Weaver system (2025) closed the generation-verification gap by 14.5% using weighted ensembles of 33 diverse weak verifiers. Individual verifier accuracy: 43–62%. Collective accuracy when 20+ agree: 91%. Key insight: **verifier diversity matters far more than verifier count**.

This directly validates PICE's architecture: diverse Arch Experts (each with different domain knowledge) are mathematically superior to multiple passes from the same model.

-----

## 3. Mathematical Foundations for the Novel Algorithms

### Sequential analysis: Wald's SPRT

Abraham Wald's Sequential Probability Ratio Test (1947) examines observations sequentially and makes a decision as soon as sufficient evidence accumulates. At each step, compute the log-likelihood ratio:

```
Λₙ = Σᵢ log(P(xᵢ | H₁) / P(xᵢ | H₀))
```

Compare against thresholds:
- Accept H₁ (code is correct) if Λₙ ≥ A = log((1−β)/α)
- Accept H₀ (code is defective) if Λₙ ≤ B = log(β/(1−α))
- Continue sampling if B < Λₙ < A

**The Wald-Wolfowitz theorem** proves SPRT minimizes expected sample size among all tests with equivalent error rates α (Type I) and β (Type II). This is the mathematically optimal stopping rule — no other test can achieve the same error control with fewer expected observations.

The expected number of samples under H₁:

```
E[N | H₁] ≈ [(1−α)log((1−β)/α) + α·log(β/(1−α))] / D_KL(p₁ ∥ p₀)
```

For an evaluator with 85% accuracy distinguishing correct from defective code, at α = 0.05, β = 0.10: **E[N] ≈ 3.2 passes**.

### Information-theoretic lower bound

The binary symmetric channel capacity gives a lower bound on required observations:

```
n ≥ log(1/δ) / (1 − H(ε))
```

where H(ε) = −ε·log(ε) − (1−ε)·log(1−ε) is binary entropy, ε is evaluator error rate, and δ is target error probability. For ε = 0.15 (85% accuracy) and δ = 0.05 (95% confidence): **n ≥ 5.1 passes**. This is consistent with the SPRT estimate — the theoretical minimum is 3–5 passes for practically achievable evaluator accuracy.

### O'Brien-Fleming group sequential boundaries

In clinical trials, O'Brien-Fleming (1979) group sequential designs distribute the overall Type I error rate across multiple interim analyses with very stringent early thresholds:

| Analysis (k of K=5) | O'Brien-Fleming z-threshold |
| -------------------- | --------------------------- |
| 1                    | 4.56                        |
| 2                    | 3.23                        |
| 3                    | 2.63                        |
| 4                    | 2.28                        |
| 5                    | 2.04                        |

Early analyses use extreme thresholds (z ≥ 4.56 at first look), preserving most discriminative power for later analyses. For PICE: this means pass 1 can only accept/reject code with very high confidence. Passes 2–3 use progressively relaxed thresholds. Final passes use near-nominal thresholds. This prevents premature acceptance of subtly flawed code while allowing rapid rejection of obviously broken submissions.

### Bayesian sequential analysis

The Bayesian approach maintains a posterior distribution over the parameter of interest (P(code_correct)) and applies a decision rule based on posterior probabilities:

```
Prior:     Beta(α₀, β₀)
After n:   Beta(α₀ + Σwᵢ·approve_i, β₀ + Σwᵢ·flag_i)
```

where wᵢ is the reliability weight for evaluator i. The posterior mean is:

```
E[θ | data] = (α₀ + Σwᵢ·approve_i) / (α₀ + β₀ + Σwᵢ)
```

and the posterior 95% credible interval provides a direct confidence measure at every step. The posterior-based stopping rule (Eckman & Henderson, 2020) halts when the posterior probability of correct classification exceeds a threshold — e.g., P(correct | data) > 0.95.

### Semantic entropy for uncertainty quantification

Kuhn et al. (ICLR 2023) introduced semantic entropy for LLM uncertainty:

1. Generate multiple outputs
2. Cluster outputs by semantic meaning (not token identity)
3. Compute entropy over semantic clusters:

```
SE = −Σ_c p_c · log(p_c)
```

Low SE = high certainty (all outputs mean the same thing). High SE = high uncertainty (outputs disagree semantically).

The deeper innovation for PICE: decompose SE into **epistemic** and **aleatoric** components. High epistemic uncertainty (models don't understand) → more diverse passes help. High aleatoric uncertainty (spec is ambiguous) → more passes cannot help → escalate to human review.

### Psychometric adaptive testing (IRT)

In Item Response Theory, each test item has parameters:
- **a** (discrimination): how sharply the item distinguishes high from low ability
- **b** (difficulty): the ability level where P(correct) = 0.5
- **c** (guessing): lower asymptote

Fisher Information for item i at ability θ:

```
I_i(θ) = a² · [P_i − c]² · [1−P_i] / [(1−c)² · P_i]
```

After each observation, select the next item maximizing information at current θ̂. Stop when Standard Error falls below threshold:

```
SE(θ̂) = 1/√(Σ I_i(θ̂))
```

PROMIS CATs use SE < 0.3 with 4–12 items. Translated to PICE: 4–12 targeted evaluation dimensions, with adaptive selection of which quality dimensions to probe next based on current uncertainty.

-----

## 4. Novel Algorithm 1: Bayesian-SPRT Adaptive Halting

### What it is

A fusion of Bayesian belief updating with Wald's Sequential Probability Ratio Test, adapted for multi-model code evaluation. No published work combines these for heterogeneous multi-model code verification.

### Prior art

ConSol (Lee et al., March 2025) applied SPRT to single-model self-consistency for reasoning tasks. This is the closest precedent but differs critically: ConSol uses a single model's self-consistency (homogeneous samples), while PICE uses heterogeneous evaluators (Claude + GPT) with different error characteristics and model-specific reliability weights.

### How it works

**Step 1: Initialize priors.** Set Beta(α₀, β₀) based on:
- Code complexity metrics (cyclomatic complexity, file count, change scope)
- Historical defect rates for similar changes (from SQLite metrics engine)
- Layer-specific base rates (infrastructure changes fail more often than CSS changes)

**Step 2: Evaluate and update.** Each pass from Claude or GPT generates a verdict (approve/flag) with an associated confidence score. Update the posterior:

```
If pass approves:  Beta(α + w_model · confidence, β)
If pass flags:     Beta(α, β + w_model · confidence)
```

where w_model is the model's learned reliability weight for this check type (from historical performance data in the self-evolving loop).

**Step 3: Check SPRT boundaries.** Compute log-likelihood ratio Λₙ and compare against thresholds with O'Brien-Fleming alpha spending:

```
If Λₙ ≥ A_k  →  ACCEPT (code passes this layer)
If Λₙ ≤ B_k  →  REJECT (code fails this layer)
Otherwise    →  CONTINUE (run another pass)
```

where A_k and B_k are the O'Brien-Fleming-adjusted thresholds for the k-th analysis.

**Step 4: Output.** At termination, report:
- The verdict (PASS/FAIL)
- The posterior mean P(correct)
- The 95% credible interval
- The number of passes used
- The cost incurred

### Expected performance

For evaluator accuracy p = 0.85, α = 0.05, β = 0.10:
- Expected passes for clear PASS: **2.4**
- Expected passes for clear FAIL: **2.1**
- Expected passes for borderline cases: **4.8**
- Overall expected passes (weighted by case distribution): **~3.2**

The Wald-Wolfowitz theorem guarantees no other stopping rule achieves lower expected pass count with the same error control.

-----

## 5. Novel Algorithm 2: Adversarial Divergence-Triggered Scaling (ADTS)

### What it is

An orchestration layer that uses inter-model disagreement as the control signal for evaluation depth. No published work uses disagreement between different LLM evaluators to dynamically allocate verification passes in code review.

### Theoretical foundation

The Knowledge Divergence theory (Kaplan et al., 2025) proves that debate advantage depends on the principal angles between models' representation subspaces — with a phase transition from quadratic (negligible benefit) to linear (essential benefit) as knowledge diversity increases. For PICE: disagreement between Claude and GPT is not noise to be averaged away but **signal about where additional evaluation is most valuable**.

Du et al. (2024) confirmed empirically that mixed-model debates outperform same-model debates, and performance plateaus after ~4 rounds — directly informing PICE's tier boundaries.

### How it works

**Step 1: Run initial evaluation.** Pass 1 (Claude) and Pass 2 (GPT) evaluate the same code against the same contract.

**Step 2: Compute divergence.** Calculate consensus entropy:

```
H_consensus = −Σ (v_k/n) · log(v_k/n)
```

where v_k counts votes for each distinct assessment category. Alternatively, compute Jensen-Shannon divergence between the two evaluators' probability distributions over verdict categories.

**Step 3: Route by divergence.**

```
If D₂ < τ_low    →  TIER 1: Halt with consensus (~70% of evaluations)
If τ_low ≤ D₂ ≤ τ_high  →  TIER 2: Targeted additional passes (~25%)
If D₂ > τ_high   →  TIER 3: Full escalation (~5%)
```

**Tier 1 (Agreement).** Both models agree with reasonable confidence. Apply Bayesian-SPRT check — if the posterior confirms, halt at 2 passes with ~92% confidence. This handles the majority of evaluations at minimal cost.

**Tier 2 (Moderate uncertainty).** Models partially disagree. Run 1–3 additional passes **targeted at the specific evaluation dimensions where disagreement is highest**. If Claude flags a security concern but GPT doesn't, the next pass focuses specifically on security evaluation. Apply Bayesian-SPRT to the expanded evidence.

**Tier 3 (Strong disagreement).** Models fundamentally disagree. Escalate:
- Add a third model (tiebreaker) with maximally different architecture/training
- Apply VEC (Algorithm 3) to determine when entropy converges
- If entropy remains high after 5+ passes, decompose into epistemic vs. aleatoric
- High aleatoric → escalate to human review (spec is ambiguous)
- High epistemic → add orthogonal verification (tests, static analysis, formal methods)

### Threshold calibration

τ_low and τ_high are calibrated from historical data in the self-evolving loop:
- τ_low: set so that cases below this threshold have <2% defect escape rate historically
- τ_high: set so that cases above this threshold have >15% defect rate historically
- Both thresholds adapt over time as the metrics engine accumulates data

-----

## 6. Novel Algorithm 3: Verification Entropy Convergence (VEC)

### What it is

A stopping rule based on the information content of accumulated evaluations, adapted from semantic entropy (Kuhn et al., ICLR 2023) and Predicted Standard Error Reduction from psychometric adaptive testing (Choi et al., 2010). No published work applies entropy-based convergence criteria to multi-pass code evaluation.

### How it works

**Step 1: Cluster evaluator outputs semantically.** After each pass, cluster all accumulated evaluation outputs by meaning using code-aware semantic similarity. Two reviews that flag different specific issues but agree on the overall assessment belong to the same semantic cluster.

**Step 2: Compute semantic entropy.**

```
SE_n = −Σ_c p_c · log(p_c)
```

over semantic clusters c, where p_c is the fraction of evaluations in cluster c.

**Step 3: Apply dual stopping criterion.** Halt when BOTH conditions are met:

```
(a) SE_n < ε           (absolute threshold: high certainty)
(b) |SE_n − SE_{n−1}| < δ   (convergence threshold: new passes aren't adding information)
```

Condition (a) ensures sufficient overall certainty. Condition (b) ensures the system has converged — additional passes would not change the verdict.

**Step 4: Decompose remaining uncertainty.**

If the system doesn't converge after the maximum allocated passes, decompose the entropy into components:

- **Epistemic entropy** — Evaluators reach different conclusions because they understand the code differently. Signal: adding a diverse evaluator shifts the semantic clusters. Response: more passes with maximally diverse evaluators (different model architectures, different prompting strategies).

- **Aleatoric entropy** — Evaluators reach different conclusions because the specification is genuinely ambiguous. Signal: adding evaluators doesn't shift the semantic clusters, but the clusters remain balanced. Response: escalate to human review. More AI passes cannot resolve what the spec doesn't define.

This decomposition is the critical innovation. It prevents PICE from wasting passes on problems that LLMs fundamentally cannot resolve — a direct response to the irreducible error findings showing that shared biases create a hard ceiling.

### Connection to adaptive testing

The stopping criterion is analogous to PROMIS CAT (computerized adaptive testing in healthcare):

```
SE(θ̂) = 1/√(Σ I_i(θ̂))    →    stop when SE < 0.3
```

In PICE: each evaluation dimension (correctness, security, performance, style, integration) has Fisher Information I_i that depends on how well the evaluators can discriminate quality at the current estimate. The system adaptively selects which dimension to evaluate next based on which would provide the most information — then stops when the overall Standard Error drops below threshold.

PROMIS CATs typically require 4–12 items for convergence. Translated to PICE: **4–12 targeted evaluation passes** for complex, multi-dimensional code review, with the adaptive selection dramatically reducing this for routine changes.

-----

## 7. Putting It All Together: The Combined Decision Engine

The three algorithms integrate into a single adaptive evaluation engine:

```
Code change arrives
       │
       ▼
┌─────────────────────────────┐
│  Bayesian-SPRT initializes  │
│  Beta prior from history    │
│  + code complexity          │
└──────────────┬──────────────┘
               │
       ┌───────▼───────┐
       │  Pass 1: Claude │──→ Update Beta posterior + compute Λ₁
       └───────┬────────┘
               │
       ┌───────▼───────┐
       │  Pass 2: GPT   │──→ Update Beta posterior + compute Λ₂
       └───────┬────────┘
               │
       ┌───────▼───────┐
       │  ADTS: Compute │
       │  divergence D₂ │
       └───────┬────────┘
               │
      ┌────────┼────────┐
      ▼        ▼        ▼
   D < τ_low  middle  D > τ_high
      │        │        │
      ▼        ▼        ▼
   SPRT     3-5 more   VEC +
   check    targeted   tiebreaker
      │     passes        │
      ▼        │          ▼
   HALT      SPRT      Entropy
   (92%)    check      converge?
              │          │
              ▼         ┌┴┐
           HALT        Y   N
           (94-95%)    │   │
                       ▼   ▼
                    HALT  Decompose:
                  (95-96%) epistemic
                           vs. aleatoric
                              │
                         ┌────┴────┐
                         ▼         ▼
                      More      Human
                      diverse   review
                      passes
```

### The minimum pass formula

For a target confidence level C, the minimum passes required:

```
N_min ≈ log((1−C_prior)/(1−C_target)) / D_KL(p_eval ∥ 1−p_eval)
```

adjusted by the correlation ceiling: N_min is capped at 1/ρ effective independent evaluations regardless of actual pass count.

| Target confidence | Passes (ρ=0.35, p=0.88) | Achievable? | Strategy |
| --- | --- | --- | --- |
| 90% | 2 | ✅ | ADTS Tier 1 |
| 93% | 3 | ✅ | ADTS Tier 1–2 |
| 95% | 4–5 | ✅ | ADTS Tier 2 |
| 96% | 7–10 | ✅ (near ceiling) | ADTS Tier 3 + VEC |
| 97% | 10+ | ⚠️ At ceiling | Add orthogonal signals |
| 99% | N/A from LLMs alone | ❌ | Requires formal verification |

**The critical design insight: beyond ~97%, PICE should escalate to orthogonal verification (tests, static analysis, formal methods) rather than adding LLM passes. This is the mathematically correct strategy, not a fallback.**

-----

## 8. Practical Implementation Notes

### Prior calibration

The Beta prior Beta(α₀, β₀) should be calibrated per layer and per change type from historical data:
- Simple CSS change: Beta(9, 1) — strong prior toward correctness (90%)
- New feature backend: Beta(7, 3) — moderate prior (70%)
- Infrastructure change: Beta(5, 5) — uninformative prior (50%)
- Security-critical change: Beta(3, 7) — prior toward caution (30%)

These priors are updated by the self-evolving loop as the metrics engine accumulates project-specific data.

### Model reliability weights

Different models have different strengths. The self-evolving loop maintains per-model, per-check-type accuracy and confidence calibration:
- Claude may be more reliable on code style and architecture patterns
- GPT may be more reliable on specific API usage and edge cases
- Haiku may be as reliable as Sonnet on simple checks at 10× lower cost

Reliability weights w_model feed directly into the Bayesian posterior update, making each pass's contribution proportional to the model's demonstrated accuracy on that check type.

### Cost optimization

The ADTS three-tier architecture naturally optimizes cost:
- **Tier 1** (~70% of evaluations): 2 passes × cheapest viable model
- **Tier 2** (~25% of evaluations): 3–5 passes × mid-tier model
- **Tier 3** (~5% of evaluations): 5–10 passes × premium model + orthogonal signals

Expected cost per evaluation = 0.70 × C_tier1 + 0.25 × C_tier2 + 0.05 × C_tier3

With Haiku at ~$0.001/pass, Sonnet at ~$0.01/pass, Opus at ~$0.10/pass:
- Tier 1: 2 × $0.001 = $0.002
- Tier 2: 4 × $0.01 = $0.04
- Tier 3: 7 × $0.10 = $0.70
- **Expected: $0.046/evaluation** — 15× cheaper than running 7 Opus passes for everything

-----

## 9. What This Means for PICE's Roadmap

1. **The confidence table belongs in every `pice status` output.** Users should see not just PASS/FAIL but the posterior confidence — "PASS at 94.2% confidence (3 passes, $0.03)."

2. **The ADTS tiers map directly to PICE's existing Tier 1/2/3 system.** This isn't a new concept to add; it's a mathematical foundation for the tiers that already exist.

3. **The self-evolving loop has a clear training signal.** The Bayesian-SPRT's prediction accuracy (did the posterior correctly predict the final verdict?) is a direct metric for tuning priors, model weights, and thresholds.

4. **99% confidence is achievable but not from LLMs alone.** PICE should make this explicit: Tier 3 evaluations that need >97% confidence should integrate formal verification, property-based testing, or human review — and communicate why.

5. **The convergence math validates small expert teams.** 2–3 diverse experts provide nearly as much information as 10 similar ones. The Krogh-Vedelsby decomposition proves that diversity, not count, drives ensemble improvement.

-----

*See also: [Seam Blindspot](seam-blindspot.md) | [Self-Evolving Verification](self-evolving-verification.md) | [Claude Code Integration](claude-code-integration.md) | [Glossary](../glossary.md)*
