---
paths:
  - "src/components/**"
  - "src/pages/**"
  - "src/app/**"
  - "**/*.tsx"
  - "**/*.css"
---

# Frontend Conventions

This file is an example on-demand rule for frontend work. Customize it for the project.

## Component Structure

- Use one component per file.
- Co-locate styles, types, and tests when the local framework supports it.
- Prefer named exports unless the project already standardizes on default exports.

```text
ComponentName/
├── ComponentName.tsx
├── ComponentName.test.tsx
└── index.ts
```

## Styling

- Use the project's established styling system.
- Avoid inline styles except for truly dynamic values.
- Follow design tokens when present.

## State Management

- Local state: `useState` or `useReducer`.
- Server state: project-standard fetch/cache library.
- Global state only when state is genuinely shared across distant surfaces.

## Forms

- Use the project's existing form library or native forms.
- Validate on both client and server when data crosses a trust boundary.
- Show validation errors near the relevant field.

## Accessibility

- Interactive elements need keyboard support.
- Images need alt text unless decorative.
- Inputs need labels.
- Prefer semantic HTML.

## Performance

- Lazy-load routes or heavy components where appropriate.
- Memoize expensive computations only when there is real cost or rerender pressure.
- Avoid introducing layout shift.
