---
description: Generate a Product Requirements Document from the current conversation
argument-hint: [output-path]
---

# Create PRD

## Overview

Transform the current conversation into a structured Product Requirements Document (PRD). This captures the full scope of work for the MVP so nothing gets lost when we move to implementation.

## Output

Write to: `$ARGUMENTS` (default: `.codex/PRD.md`)

## Before Writing

1. Review the ENTIRE conversation history — every detail matters
2. Identify any assumptions you're making and flag them
3. If critical information is missing, ask before generating

## PRD Structure

Use this template as the output structure — fill in every section with real content from the conversation:

@.codex/templates/PRD-template.md

## Quality Checks

Before saving, verify:
- [ ] Every conversation detail is captured (not just the structured parts)
- [ ] Assumptions are explicitly called out
- [ ] MVP scope is realistic (not too ambitious)
- [ ] Phases are granular enough for individual PICE loops
- [ ] Out-of-scope section prevents scope creep
- [ ] Tech stack choices are justified
- [ ] Success criteria are measurable

## After Writing

1. Confirm the file path
2. Summarize what's in the PRD
3. List any assumptions that need validation
4. Suggest reviewing the phases before starting implementation
