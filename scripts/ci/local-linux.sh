#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
platform="${PICE_LOCAL_CI_PLATFORM:-linux/amd64}"
safe_platform="${platform//\//-}"
image="${PICE_LOCAL_CI_IMAGE:-pice-local-ci:node22-rust-$safe_platform}"

run_step() {
  printf '\n==> %s\n' "$*"
  "$@"
}

run_inside_container() {
  cd "$repo_root"
  export HOME="${HOME:-/tmp/pice-local-ci-home}"
  mkdir -p "$HOME"
  export npm_config_store_dir="$HOME/.pnpm-store"
  mkdir -p "$npm_config_store_dir"
  evidence_dir="${PICE_LOCAL_CI_EVIDENCE_DIR:-$(mktemp -d -t pice-local-ci-evidence.XXXXXX)}"
  export PICE_METRICS_SCHEMA_EVIDENCE="$evidence_dir/metrics-schema-inventory.json"
  export PICE_PHASE8_REFERENCE_EVIDENCE="$evidence_dir/phase8-reference-evidence.json"
  export PICE_RELEASE_SMOKE_EVIDENCE="$evidence_dir/release-artifact-smoke-evidence.json"
  export PICE_README_MEDIA_EVIDENCE="$evidence_dir/readme-media-evidence.json"
  target_suffix="${PICE_LOCAL_CI_TARGET_SUFFIX:-linux-amd64}"
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo_root/target/local-ci-$target_suffix}"
  export PICE_BIN="$CARGO_TARGET_DIR/debug/pice"
  export PICE_DAEMON_BIN="$CARGO_TARGET_DIR/debug/pice-daemon"
  export CI=true
  export PATH="$CARGO_TARGET_DIR/debug:$PATH"
  export RUST_TEST_THREADS="${RUST_TEST_THREADS:-1}"

  printf 'Local CI evidence path: %s\n' "$evidence_dir"
  printf 'Local CI cargo target dir: %s\n' "$CARGO_TARGET_DIR"

  run_step pnpm install --frozen-lockfile --store-dir "$npm_config_store_dir"
  run_step pnpm build

  run_step cargo fmt --check
  run_step cargo clippy --workspace --all-targets -- -D warnings
  run_step cargo test --workspace --all-targets
  run_step cargo build --release

  run_step pnpm lint
  run_step pnpm typecheck
  run_step pnpm test

  run_step node scripts/acceptance/metrics-schema-inventory.mjs
  run_step node scripts/acceptance/phase8-reference-projects.mjs
  release_archive="$evidence_dir/pice-local-ci-release.tar.gz"
  run_step tar -czf "$release_archive" -C "$CARGO_TARGET_DIR/release" pice pice-daemon
  run_step env PICE_ARTIFACT_ARCHIVE="$release_archive" PICE_NPM_PACK_SMOKE=1 node scripts/acceptance/release-artifact-smoke.mjs
  run_step node scripts/acceptance/readme-media-audit.mjs
}

if [[ "${PICE_CI_IN_CONTAINER:-}" == "1" ]]; then
  run_inside_container
  exit 0
fi

run_step docker build --platform "$platform" -f "$repo_root/Dockerfile.ci" -t "$image" "$repo_root"

host_tmp_root="${PICE_LOCAL_CI_TMPDIR:-${TMPDIR:-/tmp}}"
if [[ -z "${PICE_LOCAL_CI_TMPDIR:-}" && "$(uname -s)" == "Darwin" && -d /private/tmp ]]; then
  host_tmp_root="/private/tmp"
fi
mkdir -p "$host_tmp_root"
node_modules_root="$(mktemp -d "$host_tmp_root/pice-local-ci-node-modules.XXXXXX")"
cleanup_node_modules_root() {
  rm -rf "$node_modules_root"
}
trap cleanup_node_modules_root EXIT

node_modules_targets=(
  /workspace/node_modules
  /workspace/packages/provider-base/node_modules
  /workspace/packages/provider-claude-code/node_modules
  /workspace/packages/provider-codex/node_modules
  /workspace/packages/provider-protocol/node_modules
  /workspace/packages/provider-stub/node_modules
)

node_modules_mount_args=()
for target in "${node_modules_targets[@]}"; do
  host_dir="$node_modules_root/${target//\//_}"
  mkdir -p "$host_dir"
  node_modules_mount_args+=(--mount "type=bind,source=$host_dir,target=$target")
done

docker_args=(
  run
  --rm
  --init
  --platform "$platform"
  -t
  --user "$(id -u):$(id -g)"
  -e PICE_CI_IN_CONTAINER=1
  -e RUST_TEST_THREADS=1
  -e HOME=/tmp/pice-local-ci-home
  -e PICE_LOCAL_CI_TARGET_SUFFIX="$safe_platform"
  -v "$repo_root:/workspace"
  "${node_modules_mount_args[@]}"
  -w /workspace
  "$image"
  bash scripts/ci/local-linux.sh
)

run_step docker "${docker_args[@]}"
