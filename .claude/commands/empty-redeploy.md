---
description: Trigger a platform redeploy with an empty commit when webhooks or CI connections fail
---

# Empty Commit Redeploy

Force a redeploy without code changes when the CI/CD webhook connection is acting up.

## When to Use

- Platform (Vercel, Netlify, Railway, etc.) didn't pick up a push
- CI/CD webhook missed a trigger
- Deploy is stuck and a fresh build might resolve it

## Process

```bash
git commit --allow-empty -m "chore: trigger redeploy"
git push origin main
```

That's it. The empty commit fires the platform's webhook with no code delta.
