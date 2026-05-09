---
paths:
  - "src/components/**"
  - "src/pages/**"
  - "src/app/**"
  - "**/*.tsx"
  - "**/*.css"
---

# Frontend Conventions

This file auto-loads when Claude touches frontend files. Customize for your project.

## Component Structure

- One component per file
- Co-locate styles, types, and tests with the component
- Use named exports (not default exports)

```
ComponentName/
├── ComponentName.tsx      # Component implementation
├── ComponentName.test.tsx # Tests
└── index.ts               # Re-export
```

## Styling

- Use {Tailwind / CSS Modules / styled-components}
- No inline styles except for truly dynamic values
- Follow the design tokens in `src/styles/tokens.{ext}`

## State Management

- Local state: `useState` / `useReducer`
- Server state: {React Query / SWR / RTK Query}
- Global state: {Context / Zustand / Redux} — only when truly global

## Forms

- Use {React Hook Form / Formik / native}
- Always validate on both client and server
- Show validation errors inline, below the field

## Accessibility

- All interactive elements need keyboard support
- Images need alt text
- Form inputs need labels (not just placeholders)
- Use semantic HTML elements

## Performance

- Lazy-load routes with `React.lazy()` or framework equivalent
- Memoize expensive computations with `useMemo`
- Use `useCallback` for callbacks passed to child components
- Avoid unnecessary re-renders (check with React DevTools)
