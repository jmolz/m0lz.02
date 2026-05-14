//! Hermetic Codex workflow-provider routing tests.
//!
//! These tests point `PICE_CODEX_CLI` at a cross-platform Node fixture. No
//! live Codex auth, OpenAI API key, or network call is required.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};

fn pice_cmd() -> Command {
    let mut cmd = Command::cargo_bin("pice").unwrap();
    cmd.env("PICE_DAEMON_INLINE", "1");
    cmd
}

fn fake_codex_cli(dir: &Path) -> PathBuf {
    let path = dir.join("fake-codex.mjs");
    fs::write(
        &path,
        r#"#!/usr/bin/env node
import { writeFileSync } from 'node:fs';

const HELP = `Run Codex non-interactively

Usage: codex exec [OPTIONS] [PROMPT]

Options:
  --json
  --cd <DIR>
  --sandbox <SANDBOX_MODE>
  --output-last-message <FILE>

If '-' is used, instructions are read from stdin.
`;

const args = process.argv.slice(2);
if (args.includes('--version')) {
  console.log('codex-cli 0.130.0');
  process.exit(0);
}
if (args[0] === 'exec' && args.includes('--help')) {
  console.log(HELP);
  process.exit(0);
}
if (args[0] !== 'exec') {
  console.error('unexpected command: ' + args.join(' '));
  process.exit(64);
}
let stdin = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => { stdin += chunk; });
process.stdin.on('end', () => {
  const outFlag = args.indexOf('--output-last-message');
  const outPath = outFlag >= 0 ? args[outFlag + 1] : undefined;
  if (!outPath) {
    console.error('missing --output-last-message');
    process.exit(65);
  }
  const finalText = process.env.FAKE_CODEX_FINAL || 'codex fixture final';
  writeFileSync(outPath, finalText);
  if (process.env.FAKE_CODEX_STREAM) {
    console.log(JSON.stringify({ type: 'agent_message_delta', delta: process.env.FAKE_CODEX_STREAM }));
  }
  if (process.env.FAKE_CODEX_ECHO_PROMPT === '1') {
    console.log(JSON.stringify({ type: 'agent_message_delta', delta: stdin }));
  }
  process.exit(0);
});
"#,
    )
    .unwrap();
    path
}

fn setup_codex_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let pice_dir = dir.path().join(".pice");
    fs::create_dir_all(&pice_dir).unwrap();
    fs::write(
        pice_dir.join("config.toml"),
        r#"
[provider]
name = "codex"

[evaluation.primary]
provider = "stub"
model = "stub-echo"

[evaluation.adversarial]
provider = "codex"
model = "gpt-5.5"
effort = "xhigh"
enabled = true

[evaluation.tiers]
tier1_models = ["stub-echo"]
tier2_models = ["stub-echo", "gpt-5.5"]
tier3_models = ["stub-echo", "gpt-5.5"]
tier3_agent_team = false

[telemetry]
enabled = false
endpoint = "https://telemetry.pice.dev/v1/events"

[metrics]
db_path = ".pice/metrics.db"
"#,
    )
    .unwrap();
    let fake = fake_codex_cli(dir.path());
    (dir, fake)
}

fn create_plan(dir: &Path) -> PathBuf {
    let plans = dir.join(".codex/plans");
    fs::create_dir_all(&plans).unwrap();
    let path = plans.join("codex-plan.md");
    fs::write(
        &path,
        r#"# Feature: Codex Fixture Plan

## Overview
Fixture plan.

## Contract

```json
{
  "feature": "Codex Fixture Plan",
  "tier": 2,
  "pass_threshold": 7,
  "criteria": []
}
```
"#,
    )
    .unwrap();
    path
}

fn init_git_with_modified_file(dir: &Path) {
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    fs::write(dir.join("tracked.txt"), "before\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "tracked.txt"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@test.com",
            "commit",
            "-m",
            "init",
        ])
        .current_dir(dir)
        .output()
        .unwrap();
    fs::write(dir.join("tracked.txt"), "after\n").unwrap();
}

#[test]
fn codex_provider_routes_prime_json() {
    let (dir, fake) = setup_codex_project();
    pice_cmd()
        .current_dir(dir.path())
        .env("PICE_CODEX_CLI", fake)
        .arg("prime")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""));
}

#[test]
fn codex_provider_routes_plan_json() {
    let (dir, fake) = setup_codex_project();
    pice_cmd()
        .current_dir(dir.path())
        .env("PICE_CODEX_CLI", fake)
        .arg("plan")
        .arg("add codex workflow support")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""));
}

#[test]
fn codex_provider_routes_execute_json() {
    let (dir, fake) = setup_codex_project();
    let plan = create_plan(dir.path());
    pice_cmd()
        .current_dir(dir.path())
        .env("PICE_CODEX_CLI", fake)
        .arg("execute")
        .arg(plan)
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""))
        .stdout(predicate::str::contains("Codex Fixture Plan"));
}

#[test]
fn codex_provider_routes_review_json() {
    let (dir, fake) = setup_codex_project();
    pice_cmd()
        .current_dir(dir.path())
        .env("PICE_CODEX_CLI", fake)
        .arg("review")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""));
}

#[test]
fn codex_provider_commit_dry_run_captures_final_text() {
    let (dir, fake) = setup_codex_project();
    init_git_with_modified_file(dir.path());
    pice_cmd()
        .current_dir(dir.path())
        .env("PICE_CODEX_CLI", fake)
        .env("FAKE_CODEX_FINAL", "feat(codex): fixture commit")
        .arg("commit")
        .arg("--dry-run")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("feat(codex): fixture commit"));
}

#[test]
fn codex_provider_commit_dry_run_prefers_output_last_message_over_stream() {
    let (dir, fake) = setup_codex_project();
    init_git_with_modified_file(dir.path());
    pice_cmd()
        .current_dir(dir.path())
        .env("PICE_CODEX_CLI", fake)
        .env(
            "FAKE_CODEX_STREAM",
            "streamed progress that must not become the commit message",
        )
        .env("FAKE_CODEX_FINAL", "feat(codex): authoritative final")
        .arg("commit")
        .arg("--dry-run")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("feat(codex): authoritative final"))
        .stdout(predicate::str::contains("streamed progress").not());
}

#[test]
fn codex_provider_handoff_json_writes_captured_final_text() {
    let (dir, fake) = setup_codex_project();
    pice_cmd()
        .current_dir(dir.path())
        .env("PICE_CODEX_CLI", &fake)
        .env("FAKE_CODEX_FINAL", "# Handoff\n\nCodex fixture handoff\n")
        .arg("handoff")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"path\": \"HANDOFF.md\""));

    let handoff = fs::read_to_string(dir.path().join("HANDOFF.md")).unwrap();
    assert!(handoff.contains("Codex fixture handoff"));
}
