//! `podkit`, the command line: `podkit mcp` serves MCP over stdio, and `podkit import`
//! brings projects over from Coolify, Dokploy or CapRover.

use std::process::ExitCode;

use cli::{Target, import, relay};
use tokio::io::BufReader;

const USAGE: &str = "podkit: the command line

Usage:
  podkit mcp                          serve podkit's MCP tools over stdio
  podkit import <coolify|dokploy|caprover> --url <address> [--apply]
                                      bring projects over (see podkit import --help)

Environment:
  PODKIT_URL    podkit's address (default http://127.0.0.1:8080)
  PODKIT_TOKEN  an API token (pk_...), made in Settings -> API tokens
";

fn base() -> String {
	std::env::var("PODKIT_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
	let mut args = std::env::args().skip(1);
	match args.next().as_deref() {
		Some("mcp") => {}
		Some("import") => return run_import(args.collect()).await,
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
	let client = cli::client();
	let stdin = BufReader::new(tokio::io::stdin());
	match relay(
		stdin,
		tokio::io::stdout(),
		&client,
		&Target::new(&base(), &token),
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

async fn run_import(args: Vec<String>) -> ExitCode {
	if args.iter().any(|a| a == "--help" || a == "-h") {
		print!("{}", import::USAGE);
		return ExitCode::SUCCESS;
	}
	let parsed = match import::parse(args, |k| std::env::var(k).ok()) {
		Ok(p) => p,
		Err(e) => {
			eprintln!("podkit import: {e}\n\n{}", import::USAGE);
			return ExitCode::from(2);
		}
	};
	let Ok(token) = std::env::var("PODKIT_TOKEN") else {
		eprintln!("podkit import: set PODKIT_TOKEN to an API token");
		return ExitCode::from(2);
	};
	let client = cli::client();
	let answer = match import::run(&client, &base(), &token, &parsed).await {
		Ok(a) => a,
		Err(e) => {
			eprintln!("podkit import: {e}");
			return ExitCode::FAILURE;
		}
	};
	if parsed.json {
		println!("{}", answer.body);
	} else {
		let label = match parsed.source.as_str() {
			"coolify" => "Coolify",
			"dokploy" => "Dokploy",
			_ => "CapRover",
		};
		print!("{}", import::render(label, &answer));
	}
	if import::ok(&answer) {
		ExitCode::SUCCESS
	} else {
		ExitCode::FAILURE
	}
}
