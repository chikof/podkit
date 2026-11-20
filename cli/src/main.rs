//! `podkit`, the command line. Today it has one command: `podkit mcp`.

use std::process::ExitCode;

use cli::{Target, relay};
use tokio::io::BufReader;

const USAGE: &str = "podkit mcp: serve podkit's MCP tools over stdio

Usage: podkit mcp

Environment:
  PODKIT_URL    podkit's address (default http://127.0.0.1:8080)
  PODKIT_TOKEN  an API token (pk_...), made in Settings -> API tokens
";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
	let mut args = std::env::args().skip(1);
	match args.next().as_deref() {
		Some("mcp") => {}
		Some("--version" | "-V") => {
			println!("podkit {}", env!("CARGO_PKG_VERSION"));
			return ExitCode::SUCCESS;
		}
		Some("--help" | "-h") | None => {
			print!("{USAGE}");
			return ExitCode::SUCCESS;
		}
		Some(other) => {
			eprintln!("podkit: unknown command {other}\n\n{USAGE}");
			return ExitCode::from(2);
		}
	}
	let Ok(token) = std::env::var("PODKIT_TOKEN") else {
		eprintln!("podkit mcp: set PODKIT_TOKEN to an API token\n\n{USAGE}");
		return ExitCode::from(2);
	};
	let base = std::env::var("PODKIT_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned());
	let client = cli::client();
	let stdin = BufReader::new(tokio::io::stdin());
	match relay(
		stdin,
		tokio::io::stdout(),
		&client,
		&Target::new(&base, &token),
	)
	.await
	{
		Ok(()) => ExitCode::SUCCESS,
		Err(e) => {
			eprintln!("podkit mcp: {e}");
			ExitCode::FAILURE
		}
	}
}
