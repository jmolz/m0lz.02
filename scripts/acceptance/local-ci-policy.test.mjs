import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

function read(relativePath) {
  return readFileSync(path.join(repoRoot, relativePath), 'utf8').replace(/\r\n/g, '\n');
}

describe('local CI policy', () => {
  it('keeps the Linux Docker preflight aligned with CI daemon-test requirements', () => {
    const script = read('scripts/ci/local-linux.sh');
    expect(script).toContain('platform="${PICE_LOCAL_CI_PLATFORM:-linux/amd64}"');
    expect(script).toContain('docker build --platform "$platform" -f "$repo_root/Dockerfile.ci"');
    expect(script).toContain('--platform "$platform"');
    expect(script).toContain('node_modules_root="$(mktemp -d "$host_tmp_root/pice-local-ci-node-modules.XXXXXX")"');
    expect(script).toContain('trap cleanup_node_modules_root EXIT');
    expect(script).toContain('/workspace/node_modules');
    expect(script).toContain('/workspace/packages/provider-base/node_modules');
    expect(script).toContain('/workspace/packages/provider-codex/node_modules');
    expect(script).toContain('node_modules_mount_args+=(--mount "type=bind,source=$host_dir,target=$target")');
    expect(script).toContain('export npm_config_store_dir="$HOME/.pnpm-store"');
    expect(script).toContain('pnpm install --frozen-lockfile --store-dir "$npm_config_store_dir"');
    expect(script).toContain('export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo_root/target/local-ci-$target_suffix}"');
    expect(script).toContain('export PICE_BIN="$CARGO_TARGET_DIR/debug/pice"');
    expect(script).toContain('export PICE_DAEMON_BIN="$CARGO_TARGET_DIR/debug/pice-daemon"');
    expect(script).toContain('export PATH="$CARGO_TARGET_DIR/debug:$PATH"');
    expect(script).toContain('export RUST_TEST_THREADS="${RUST_TEST_THREADS:-1}"');
    expect(script).toContain('PICE_PHASE8_REFERENCE_EVIDENCE');
    expect(script).toContain('PICE_README_MEDIA_EVIDENCE');
    expect(script).toContain('cargo test --workspace --all-targets');
    expect(script).toContain('node scripts/acceptance/phase8-reference-projects.mjs');
    expect(script).toContain('tar -czf "$release_archive" -C "$CARGO_TARGET_DIR/release" pice pice-daemon');
    expect(script).toContain('PICE_ARTIFACT_ARCHIVE="$release_archive" PICE_NPM_PACK_SMOKE=1 node scripts/acceptance/release-artifact-smoke.mjs');
    expect(script).toContain('node scripts/acceptance/readme-media-audit.mjs');
  });

  it('builds the local CI image with Node 22, Rust stable, rustfmt, clippy, and pnpm 9', () => {
    const dockerfile = read('Dockerfile.ci');
    expect(dockerfile).toContain('FROM node:22-bookworm');
    expect(dockerfile).toContain('--default-toolchain stable');
    expect(dockerfile).toContain('rustup component add rustfmt clippy');
    expect(dockerfile).toContain('npm install -g pnpm@9');
  });

  it('exposes a manual hosted Windows smoke workflow for named-pipe and cmd wrapper behavior', () => {
    const workflow = read('.github/workflows/windows-smoke.yml');
    expect(workflow).toContain('workflow_dispatch:');
    expect(workflow).toContain('runs-on: windows-latest');
    expect(workflow).toContain('scripts/ci/windows-smoke.ps1');
  });

  it('keeps the wall-clock cohort speedup gate on Linux and out of Windows platform coverage', () => {
    const workflow = read('.github/workflows/ci.yml');
    expect(workflow).toContain('cargo test --workspace --all-targets');
    expect(workflow).toContain('The Linux rust job is the authoritative gate for wall-clock');
    expect(workflow).toContain("if: runner.os == 'Windows'");
    expect(workflow).toContain('cargo test -- --skip parallel_cohort_meets_16x_speedup');
  });

  it('keeps the native Windows smoke script focused on Windows-only release risks', () => {
    const script = read('scripts/ci/windows-smoke.ps1');
    expect(script).toContain('$env:PATH = "$DebugBin;$env:PATH"');
    expect(script).toContain('$env:RUST_TEST_THREADS = "1"');
    expect(script).toContain('Invoke-Step cargo @("test", "--", "--skip", "parallel_cohort_meets_16x_speedup")');
    expect(script).toContain('Invoke-Step cargo @("build", "--release", "-p", "pice-cli", "-p", "pice-daemon")');
    expect(script).toContain('release-artifact-smoke.test.mjs');
    expect(script).toContain('PICE_RELEASE_SMOKE_EVIDENCE');
    expect(script).toContain('$env:PICE_NPM_PACK_SMOKE = "1"');
    expect(script).toContain('release-artifact-smoke.mjs');
  });
});
