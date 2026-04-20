pub mod audit;
pub mod benchmark;
pub mod commit;
pub mod daemon;
pub mod evaluate;
pub mod execute;
pub mod handoff;
pub mod init;
pub mod layers;
pub mod metrics;
pub mod plan;
pub mod prime;
pub mod review;
pub mod review_gate;
pub mod status;
pub mod validate;

use anyhow::Result;
use pice_core::cli::CommandResponse;

/// Render a [`CommandResponse`] to the terminal.
///
/// Every command's `run()` calls this after `adapter::dispatch()`. The
/// response variant already encodes the format (JSON vs. text) because the
/// daemon handler checked `req.json` when building the response.
pub fn render_response(resp: CommandResponse) -> Result<()> {
    match resp {
        CommandResponse::Json { value } => {
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        CommandResponse::Text { content } => {
            println!("{content}");
        }
        CommandResponse::Empty => {}
        CommandResponse::Exit { code, message } => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            std::process::exit(code);
        }
        CommandResponse::ExitJson { code, value } => {
            // Structured JSON-mode failure: emit to stdout so machine callers
            // (e.g. `pice validate --json && deploy`) can parse the report,
            // then exit nonzero so the shell chain fails closed.
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
            );
            std::process::exit(code);
        }
    }
    Ok(())
}
