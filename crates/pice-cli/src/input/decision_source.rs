//! Reviewer prompt rendering for the `pice review-gate` TTY flow.
//!
//! Phase 6 initially shipped a `DecisionSource` trait with three impls
//! (Scripted / Piped / Tty) intended to abstract the reviewer-input
//! source. In practice `StdinLock: !Send` blocked the trait from being
//! wired through the async handler path, and both production call sites
//! (`commands/review_gate.rs::prompt_tty_for_decision` + `commands/
//! evaluate.rs::prompt_decision_for_gate`) ended up reading stdin
//! directly while emitting the prompt via this helper. The trait was
//! pure scaffolding debt — the review-gate eval (Pass 3) flagged it —
//! so it's been removed. `render_prompt` is the only surviving export.
//!
//! When Phase 7 re-introduces a true async-friendly input abstraction
//! (blocking reads wrapped via `tokio::task::spawn_blocking` or a
//! PTY-shaped mock for test harnessing), reconstruct the trait layer at
//! that time — don't ship the abstraction ahead of a real consumer.

/// Render the Unicode box-drawing prompt. Pure — testable without
/// stdio mocking. Callers write the returned string to stderr (the
/// "Channel ownership invariant" in `.claude/rules/daemon.md` reserves
/// stdout for daemon-emitted streaming + JSON responses).
pub fn render_prompt(body: &str, details: Option<&str>) -> String {
    // Fixed interior width so visually the box doesn't jitter with
    // the body's line count.
    const W: usize = 57;
    let top = format!(
        "\u{2554}═══ REVIEW GATE {}\u{2557}",
        "═".repeat(W.saturating_sub(17))
    );
    let bottom = format!("\u{255a}{}\u{255d}", "═".repeat(W));
    let sep = format!("\u{2560}{}\u{2563}", "═".repeat(W));
    let body_lines = wrap_to(body, W - 2);
    let mut out = String::new();
    out.push_str(&top);
    out.push('\n');
    for line in body_lines {
        out.push_str(&format!(
            "\u{2551} {:<width$}\u{2551}\n",
            line,
            width = W - 1
        ));
    }
    if let Some(d) = details {
        out.push_str(&sep);
        out.push('\n');
        for line in wrap_to(d, W - 2) {
            out.push_str(&format!(
                "\u{2551} {:<width$}\u{2551}\n",
                line,
                width = W - 1
            ));
        }
    }
    out.push_str(&sep);
    out.push('\n');
    out.push_str(&format!(
        "\u{2551} {:<width$}\u{2551}\n",
        "[a]pprove  [r]eject  [d]etails  [s]kip",
        width = W - 1
    ));
    out.push_str(&bottom);
    out
}

fn wrap_to(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    for line in text.lines() {
        if line.chars().count() <= width {
            out.push(line.to_string());
            continue;
        }
        let mut buf = String::new();
        let mut count = 0;
        for ch in line.chars() {
            if count == width {
                out.push(std::mem::take(&mut buf));
                count = 0;
            }
            buf.push(ch);
            count += 1;
        }
        if !buf.is_empty() {
            out.push(buf);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_prompt_includes_details_when_provided() {
        let prompt = render_prompt("gate body", Some("detail block"));
        assert!(prompt.contains("gate body"));
        assert!(prompt.contains("detail block"));
        assert!(prompt.contains("[a]pprove"));
    }

    #[test]
    fn render_prompt_omits_detail_separator_when_none() {
        let prompt = render_prompt("gate body", None);
        // Exactly one separator row (between body and action line).
        let sep_count = prompt.lines().filter(|l| l.starts_with('\u{2560}')).count();
        assert_eq!(sep_count, 1);
    }
}
