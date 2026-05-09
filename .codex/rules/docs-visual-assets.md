---
paths:
  - "docs/images/**"
  - "docs/diagrams/**"
  - "README.md"
---

# Visual Assets Rules

## SVG Authoring (GitHub-Compatible)

GitHub's SVG sanitizer is strict. Follow these constraints:

- **No `<style>` tags** — GitHub strips them. Use inline attributes only.
- **No `@font-face` or `@media` queries** — GitHub strips them from SVGs.
- **No external references** — no `xlink:href` to external URLs, no `@import`, no `url()` pointing outside the file.
- **`<defs>` must precede usage** — place `<defs>` immediately after the opening `<svg>` tag, before any elements that reference markers/filters. GitHub's renderer does not reliably handle forward references.
- **Use system font stacks** — `'Segoe UI', Roboto, 'Helvetica Neue', sans-serif` for body text, `'SF Mono', 'Fira Code', 'Consolas', monospace` for code.
- **Use `viewBox`** for responsive sizing — no fixed `width`/`height` on the root `<svg>`.
- **Include `<title>` and `<desc>`** for accessibility.
- **Internal `url(#fragment)` references are fine** — these reference `<marker>` and `<filter>` elements in the same file's `<defs>`. They are self-contained.

## Dark Mode

GitHub dark mode requires two separate SVG files (light + dark) referenced via `<picture>`:

```html
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/foo-dark.svg">
  <img alt="Description" src="docs/images/foo-light.svg" width="800">
</picture>
```

### Color guidelines

- **Light variant**: white background (`#ffffff`), dark text (`#1e293b` or darker)
- **Dark variant**: GitHub dark background (`#0d1117`), light text (`#e2e8f0` or lighter)
- **Both**: WCAG AA contrast ratio (4.5:1 minimum for text at any size)
- **Same layout and proportions** — only colors differ between variants.

## GIF Recording (VHS)

- **Mock scripts** (`docs/images/*.sh`) echo exact CLI output without requiring API keys — keeps GIF generation reproducible.
- **VHS tape files** (`docs/images/*.tape`) must be run from the repository root directory.
- **Target < 2MB** for GIFs. Use `Set Framerate 15` and compact dimensions.
- **Use `Hide`/`Show` + `clear`** to conceal setup commands (shell functions, aliases) from the recording.

## Excalidraw Sources

- `.excalidraw` files in `docs/diagrams/` are the editable design source for SVGs.
- Use `roughness: 0` for clean modern style.
- Follow semantic colors from `.codex/skills/excalidraw-diagram/references/color-palette.md`.
- SVGs are hand-crafted from the Excalidraw design, not exported — this ensures GitHub compatibility.

## Alt Text

All `<img>` tags in README must have descriptive `alt` attributes that convey the visual content to screen readers. Not just labels — describe what the image shows.
