---
description: Build, test, commit by feature, and deploy — adapts to any project's toolchain
---

Run the migrated `source-command-commit-and-deploy` skill with arguments: $ARGUMENTS

Treat this slash command as the Codex entrypoint for the former Claude project command `commit-and-deploy.md`.

This entrypoint inherits the mandatory README review/update gate from the migrated skill: every deployment must verify `README.md` freshness, update it before push/tag when it drifted, and report the README evidence used.
