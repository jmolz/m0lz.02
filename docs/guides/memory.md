# PICE Memory

PICE memory is optional, summary-only context for workflow commands. It records
redacted lessons after successful lifecycle points and recalls a bounded brief only
for commands that are allowed to use it.

Memory is off by default:

```toml
[memory]
enabled = false
store = "project_learnings"
max_recalled_items = 6
max_tokens = 1200
retention_days = 90
write_after = ["execute", "handoff"]
read_for = ["prime", "plan", "execute"]
```

## Stores

`project_learnings` writes Markdown records to `.pice/learnings.md`. This is useful
for durable, reviewable project lessons that should travel with the repo.

`private_state` writes JSONL records to
`~/.pice/state/{project_hash}/memory/records.jsonl`. This is useful for local,
non-committed reminders.

`both` writes each approved record to both stores.

Project learning records are machine-readable Markdown blocks. Each block carries
metadata including a stable `mem_` id, source phase, project hash, and
`redaction_status`:

```markdown
<!-- pice-memory id="mem_..." created_at="2026-05-19T00:00:00Z" source="handoff_summary" store="project_learnings" project_hash="..." redaction_status="clean" -->
### Short durable lesson

Redacted summary body.
<!-- /pice-memory -->
```

## Isolation

Recalled memory may be added to `pice prime`, `pice plan`, and `pice execute`.
It is never added to `pice review`, `pice evaluate`, adversarial evaluation, or
`pice commit`. Plans, contracts, manifests, and staged diffs remain the source of
truth for correctness.

PICE stores summaries, not raw transcripts. The daemon rejects memory records that
look like secrets, raw diffs, raw command transcripts, long stack traces, or entries
that exceed the configured token budget.

## Governance Commands

```bash
pice memory status
pice memory list --limit 10
pice memory list --feature my-feature-id
pice memory show <record-id>
pice memory prune --before 2026-05-19
pice memory delete <record-id>
```

`prune` without `--before` uses `retention_days`. Set `retention_days = 0` to
disable retention-based pruning and require an explicit `--before` boundary.

Deleting or pruning `.pice/learnings.md` edits the current file only. Prior git
history may still contain old records, so use `private_state` for local-only
memory that should not be committed.
