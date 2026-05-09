# Architecture Deep Dive

> **Purpose**: Heavy reference for sub-agent scouts and deep planning sessions. NOT auto-loaded — only brought in when needed.

## System Overview

{Describe the full system architecture here. This is where you put the detail that's too heavy for AGENTS.md but essential for understanding how things fit together.}

## Data Flow

```
{Diagram or description of how data moves through the system}

Request → Middleware → Router → Controller → Service → Repository → Database
                                    ↓
                              Event Emitter → WebSocket → Client
```

## Package/Module Boundaries

{For monorepos or large projects, describe what each package owns and the dependency rules between them.}

| Package | Owns | May Import From | Must NOT Import From |
|---------|------|-----------------|---------------------|
| {core} | Business logic | {utils} | {ui, api} |
| {api} | HTTP layer | {core, utils} | {ui} |
| {ui} | Frontend | {core (types only)} | {api internals} |

## Database Schema

{Key tables, relationships, and conventions. Not the full migration history — just what an agent needs to understand the data model.}

## Authentication & Authorization

{How auth works end-to-end. Token flow, session management, permission model.}

## Error Handling Strategy

{How errors propagate through the system. Error types, classification, user-facing messages.}

## Environment & Configuration

{How configuration is loaded, what env vars control, feature flags.}

## Deployment Architecture

{How the app is deployed, infrastructure overview, CI/CD pipeline.}
