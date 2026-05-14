import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    globals: true,
    include: [
      'packages/*/src/**/*.test.ts',
      'packages/*/src/**/__tests__/**/*.ts',
      'npm/**/*.test.mjs',
      'scripts/acceptance/**/*.test.mjs',
    ],
  },
});
