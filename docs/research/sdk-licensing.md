# Claude Agent SDK Licensing: Analysis for Open Source Integration

*PICE Research Library — Expanded research supporting the [PICE Roadmap](../roadmap.md)*

*For term definitions, see the [Glossary](../glossary.md).*

-----

## Executive Summary

The Claude Agent SDK has a complex, layered licensing structure that creates real constraints for PICE's open source distribution. The Python SDK wrapper code carries a standard MIT license, but the TypeScript SDK is proprietary, and both packages bundle the proprietary Claude Code CLI binary. PICE's recommended approach — CLI subprocess invocation — sidesteps SDK licensing concerns entirely by treating Claude Code as an external tool invoked over stdio, the same way any program invokes any CLI utility.

-----

## 1. The Licensing Landscape

### Python SDK: MIT Licensed

**Package:** `claude-agent-sdk` on PyPI
**Repository:** `anthropics/claude-agent-sdk-python` on GitHub
**License file:** Standard MIT License — the full permissive text granting rights to "use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies." Copyright 2025 Anthropic, PBC.
**PyPI classifier:** `OSI Approved :: MIT License`

The Python SDK's wrapper code — the Python interface layer that communicates with the Claude Code CLI — is genuinely open source under MIT. PICE can freely depend on, modify, and redistribute this code.

**Historical note:** The predecessor package `claude-code-sdk` (now deprecated, last version 0.0.25) was also MIT-licensed. When Anthropic rebranded to `claude-agent-sdk`, the Python wrapper retained MIT.

### TypeScript SDK: Proprietary

**Package:** `@anthropic-ai/claude-agent-sdk` on npm
**Repository:** `anthropics/claude-agent-sdk-typescript` on GitHub
**License field (npm):** `SEE LICENSE IN README.md` — a non-standard, non-SPDX identifier
**LICENSE.md contents:** Single line, ~150 bytes: `© Anthropic PBC. All rights reserved. Use is subject to Anthropic's Commercial Terms of Service.`
**GitHub license badge:** None displayed (GitHub does not recognize the license as open source)

Third-party documentation from Promptfoo explicitly describes it as a "proprietary license." The TypeScript SDK is not open source.

### Claude Code CLI Binary: Proprietary

**Package:** `@anthropic-ai/claude-code` on npm
**LICENSE.md:** `© Anthropic PBC. All rights reserved. Use is subject to Anthropic's Commercial Terms of Service.`
**Package size:** ~45.3 MB (largely the bundled binary)

Despite its public GitHub repository (94,700+ stars as of April 2026), Claude Code has never been open source. The repository is source-available for inspection but all rights are reserved.

### The bundled binary problem

Both SDK packages automatically include the Claude Code CLI binary within their distributions:
- The Python SDK ships platform-specific wheels containing the binary (Linux x86_64, Linux aarch64, macOS x86_64, macOS ARM64)
- The TypeScript package's 45.3 MB size is largely the bundled binary

This creates a layered licensing situation:
- **Python wrapper code:** MIT — freely redistributable
- **Bundled CLI binary inside the Python package:** Proprietary — cannot be extracted and redistributed independently
- **TypeScript SDK as a whole:** Proprietary

### The README disclaimer

Both SDKs' README files contain identical language in their "License and terms" sections:

> *Use of this SDK is governed by Anthropic's Commercial Terms of Service, including when you use it to power products and services that you make available to your own customers and end users, except to the extent a specific component or dependency is covered by a different license as indicated in that component's LICENSE file.*

This creates a dual layer: the Python wrapper code is MIT (as indicated by its LICENSE file), but using the SDK as a whole — including the bundled CLI binary — triggers the Commercial Terms of Service.

-----

## 2. Anthropic's Enforcement Posture

### DMCA enforcement

Anthropic has actively defended Claude Code's proprietary status. In April 2025, the company filed **DMCA takedown notices** against developers who reverse-engineered and deobfuscated the CLI's source code. TechCrunch reported on the incident, noting the tension between Claude Code's public GitHub presence and its proprietary license.

### The March 2026 source leak

Claude Code's entire source code (~512,000 lines across 1,900 files) was accidentally leaked through an npm packaging error that included unstripped source maps. The code was forked over **41,500 times** before Anthropic issued takedowns. Anthropic's official statement: "a release packaging issue caused by human error, not a security breach."

Critically, this leak did not change the proprietary status. Using, copying, or redistributing that code remains a license violation regardless of its accidental public availability. Community projects built from the leaked code (such as `claw-code` by Sigrid Jin, which reached 30,000 stars) operate in legally uncertain territory.

### Open source licensing requests

GitHub issue #22002 on the Claude Code repository is a feature request to open-source the CLI under a permissive license (Apache 2.0 or MIT). It references widespread user confusion (issues #333, #1645, #1789, #19073) about the repository being "open source" when it is not. In March 2025, the Claude Code team stated they **"weren't ready to be good public stewards yet."**

GitHub issue #8517 flagged that Claude Code binaries contain Apache-2.0-licensed open source dependencies without proper attribution — a potential compliance issue for Anthropic itself.

-----

## 3. Anthropic's Commercial Terms of Service

### Key provisions affecting PICE

**API key requirement.** Anthropic explicitly prohibits third-party developers from offering `claude.ai` login or routing requests through Free, Pro, or Max plan credentials. PICE users must authenticate with their own API keys through Claude Console or a supported cloud provider (AWS Bedrock, Google Cloud Vertex AI).

**Branding restrictions.** Partners may use:
- ✅ "Claude Agent" or "{YourAgentName} Powered by Claude"
- ❌ "Claude Code" or "Claude Code Agent" branding
- ❌ Claude Code-branded ASCII art

PICE must not present itself as a Claude Code product or use Anthropic's trademarks.

**Usage Policy compliance.** All users must comply with Anthropic's Usage Policy, which prohibits:
- Model scraping or distillation
- Jailbreaking or bypassing safety guardrails
- Using API outputs to train competing AI models without authorization

PICE's integration must not facilitate any of these uses.

**Copyright indemnity.** Anthropic defends commercial customers against copyright infringement claims for authorized API use. This protection applies to PICE users who access Claude through the API with their own keys.

**Data collection.** The SDK collects usage telemetry including code acceptance/rejection rates, conversation data, and user feedback. PICE's documentation should disclose this to users.

### Claude for Open Source program

Anthropic runs a "Claude for Open Source" program offering eligible OSS maintainers free Claude Max access for six months. This concerns *using* Claude as a tool, not open-sourcing Claude's own code. PICE maintainers could apply for this program to support development.

-----

## 4. PICE's Recommended Approach

### Option B: CLI subprocess (chosen)

PICE spawns `claude --bare -p` as a subprocess from Rust, communicating via JSON-lines over stdio. This is the same mechanism the TypeScript SDK uses internally, but without taking a dependency on the SDK package.

**Why this works for open source:**

1. **No compile-time dependency on proprietary code.** PICE's codebase contains zero lines of Anthropic-proprietary code. The integration is purely a runtime invocation of an external CLI tool — identical in legal character to a script that invokes `git`, `docker`, or `curl`.

2. **No redistribution of proprietary binaries.** PICE does not ship the Claude Code CLI. Users install it independently via `npm install -g @anthropic-ai/claude-code`, accepting Anthropic's terms in the process.

3. **PICE works without Claude Code.** The integration is an optional provider. The core PICE framework — Stack Loops, seam verification, adaptive algorithms, metrics engine — functions independently. Claude Code is one of potentially many execution substrates.

4. **Clean license boundary.** PICE's license (MIT or Apache 2.0) applies to PICE's code. Anthropic's Commercial Terms apply to Claude Code's code. The boundary is clear: they interact over stdio, not through linked libraries.

### If PICE later wants an SDK dependency

The Python SDK (`claude-agent-sdk`) is the safer choice:
- Its wrapper code is MIT-licensed — compatible with any open source license
- Declare as an **optional dependency** (e.g., `pip install pice[claude]`)
- The bundled CLI binary remains proprietary, but users accept those terms when they install the package
- The core framework stays free of proprietary entanglements

The TypeScript SDK should be avoided for any direct dependency due to its proprietary license.

### Documentation requirements

PICE's documentation must clearly state:

1. Using PICE's Claude Code integration requires a separate Claude Code installation and Anthropic API key
2. Claude Code is proprietary software governed by Anthropic's Commercial Terms of Service
3. Users are responsible for compliance with Anthropic's Usage Policy
4. The Claude Code CLI collects usage telemetry
5. PICE is not affiliated with, endorsed by, or a product of Anthropic

-----

## 5. License Compatibility Matrix

| PICE License | Python SDK (MIT) | TypeScript SDK (Proprietary) | CLI Binary (Proprietary) | CLI Subprocess |
|---|---|---|---|---|
| MIT | ✅ Compatible as optional dep | ❌ Cannot bundle/require | ❌ Cannot redistribute | ✅ No license conflict |
| Apache 2.0 | ✅ Compatible as optional dep | ❌ Cannot bundle/require | ❌ Cannot redistribute | ✅ No license conflict |
| GPL v3 | ✅ Compatible (MIT is GPL-compat) | ❌ Cannot link | ❌ Cannot link | ✅ Subprocess = separate program |
| AGPL v3 | ✅ Compatible | ❌ Cannot link | ❌ Cannot link | ✅ Subprocess = separate program |

The CLI subprocess approach (Option B) is license-compatible with every open source license because subprocess invocation does not create a derivative work. This is the same legal principle that allows GPL software to invoke proprietary compilers, or MIT software to invoke proprietary databases.

-----

## 6. Risk Assessment

### Low risk

- **CLI subprocess invocation.** Well-established legal principle. No court has held that invoking a program via its public CLI creates a license obligation on the invoking program.
- **Python SDK as optional dependency.** MIT license is maximally permissive. The proprietary binary is installed by the user, not redistributed by PICE.

### Medium risk

- **API changes.** Anthropic could change the CLI's `--bare`, `--output-format`, or `--agents` flags without notice, breaking PICE's integration. Mitigation: pin to known-good CLI versions, implement graceful degradation.
- **Terms of Service changes.** Anthropic could modify its Commercial Terms to restrict CLI subprocess invocation by third-party tools. Unlikely (this would break the entire ecosystem) but theoretically possible.

### Low but notable risk

- **Branding confusion.** Users might perceive PICE as a Claude Code product. Mitigation: clear branding separation, explicit disclaimers, no Anthropic trademarks in PICE materials.
- **Telemetry concerns.** Users may not realize the Claude Code CLI collects usage data when PICE invokes it. Mitigation: document in PICE's privacy/data section.

### Mitigated risk

- **Proprietary license contamination.** By using CLI subprocess (not SDK dependency), PICE's codebase remains entirely under its own license. No proprietary code is compiled into, linked with, or distributed alongside PICE.

-----

## 7. Comparison with Similar Open Source Projects

Many successful open source projects integrate with proprietary tools via CLI subprocess without licensing issues:

| Project | Proprietary tool | Integration method | License conflict? |
|---|---|---|---|
| Terraform | AWS CLI, Azure CLI, GCP CLI | CLI subprocess + API | No |
| Docker Compose | Docker Engine | CLI subprocess | No |
| Homebrew | macOS system tools | CLI subprocess | No |
| VS Code extensions | VS Code (MIT, but MS-proprietary builds) | Extension API | Debated, generally no |
| **PICE** | **Claude Code CLI** | **CLI subprocess** | **No** |

PICE's approach follows the same well-trodden pattern: an open source orchestrator that invokes proprietary tools as external dependencies, with users responsible for installing and licensing those tools independently.

-----

## 8. Summary of Recommendations

1. **Use CLI subprocess integration (Option B).** No compile-time dependency on proprietary code. Clean license boundary. Maximum open source compatibility.

2. **Make Claude Code optional.** PICE's core framework must work without Claude Code installed. The integration activates when the CLI is available.

3. **Users bring their own installation and API keys.** PICE never distributes the CLI binary. Users install independently and accept Anthropic's terms.

4. **If an SDK dependency is needed later, use Python.** MIT-licensed wrapper is compatible with any PICE license. Declare as optional.

5. **Avoid the TypeScript SDK.** Proprietary license creates redistribution and bundling concerns for open source.

6. **Maintain clear branding separation.** No Anthropic trademarks. Explicit disclaimer that PICE is independent.

7. **Document the data implications.** Users should know the CLI collects telemetry when PICE invokes it.

8. **Consider applying for Claude for Open Source.** Free Claude Max access for PICE maintainers could support development without licensing concerns.

-----

*See also: [Claude Code Integration](claude-code-integration.md) | [Seam Blindspot](seam-blindspot.md) | [Glossary](../glossary.md)*
