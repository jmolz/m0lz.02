#!/usr/bin/env node
import { CodexProvider } from './index.js';
import { installCodexChildSignalHandlers } from './workflow.js';

installCodexChildSignalHandlers();
const provider = new CodexProvider();
provider.start().catch((err) => {
  console.error('Codex provider failed:', err);
  process.exit(1);
});
