//! `podkit`, the command line: log in, list, deploy, follow logs and sync variables over
//! the JSON API; `podkit mcp` serves MCP over stdio, and `podkit import` brings projects
//! over from Coolify, Dokploy or CapRover.

use std::path::PathBuf;
use std::process::ExitCode;

use cli::api::{EXIT_USAGE, Fail};
use cli::args::{self, Command, USAGE};
use cli::commands::{self, Ctx, Push};
use cli::config::{self, Settings};
use cli::{Target, import, relay};
use tokio::io::BufReader;

fn env(k: &str) -> Option<String> {
	std::env::var(k).ok()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
	let command = match args::parse(std::env::args().skip(1).collect()) {
		Ok(c) => c,
		Err(e) => {
			eprintln!("podkit: {e}");
			return ExitCode::from(EXIT_USAGE);
		}
	};
	match run(command).await {
		Ok(()) => ExitCode::SUCCESS,
		Err(f) => {
			eprintln!("podkit: {}", f.message);
			ExitCode::from(f.code)
		}
	}
}

fn config_path() -> Result<PathBuf, Fail> {
	config::path(env).ok_or_else(|| {
		Fail::usage("no config directory: set HOME, XDG_CONFIG_HOME or PODKIT_CONFIG")
	})
}

/// The merged settings: flags, then the environment, then the config file.
fn settings(team: Option<&str>, project: Option<&str>) -> Result<Settings, Fail> {
	// No config directory is fine as long as the environment says everything.
	let file = match config::path(env) {
		Some(p) => config::load(&p).map_err(Fail::usage)?,
		None => config::Config::default(),
	};
	Ok(config::resolve(&file, env, team, project))
}

fn ctx(place: &args::Place) -> Result<Ctx, Fail> {
	Ctx::new(settings(place.team.as_deref(), place.project.as_deref())?)
}

async fn run(command: Command) -> Result<(), Fail> {
	match command {
		Command::Help(None) => print!("{USAGE}"),
		Command::Help(Some(c)) if c == "import" => print!("{}", import::USAGE),
		Command::Help(Some(c)) => match args::help(&c) {
			Some(h) => print!("{h}"),
			None => return Err(Fail::usage(format!("unknown command {c}\n\n{USAGE}"))),
		},
		Command::Version => println!("podkit {}", env!("CARGO_PKG_VERSION")),
		Command::Mcp => return mcp().await,
		Command::Import(args) => return run_import(args).await,
		Command::Login { url, token, place } => {
			let current = settings(None, None)?;
			commands::login(&config_path()?, &current, url, token, place).await?;
		}
		Command::Logout => commands::logout(&config_path()?)?,
		Command::Whoami { json } => commands::whoami(&ctx(&args::Place::default())?, json).await?,
		Command::Apps { place, json } => commands::apps(&ctx(&place)?, &place, json).await?,
		Command::Deployments {
			app,
			place,
			limit,
			json,
		} => commands::deployments(&ctx(&place)?, &place, &app, limit, json).await?,
		Command::Deploy {
			app,
			place,
			wait,
			json,
		} => commands::deploy(&ctx(&place)?, &place, &app, wait, json).await?,
		Command::Rollback {
			app,
			place,
			to,
			wait,
			json,
		} => commands::rollback(&ctx(&place)?, &place, &app, to, wait, json).await?,
		Command::Logs {
			app,
			place,
			follow,
			tail,
		} => commands::logs(&ctx(&place)?, &place, &app, follow, tail).await?,
		Command::EnvPull {
			app,
			place,
			file,
			force,
		} => commands::env_pull(&ctx(&place)?, &place, &app, &file, force).await?,
		Command::EnvPush {
			app,
			place,
			file,
			yes,
			prune,
			allow_empty,
			dry_run,
		} => {
			let push = Push {
				file: &file,
				yes,
				prune,
				allow_empty,
				dry_run,
			};
			commands::env_push(&ctx(&place)?, &place, &app, push).await?;
		}
	}
	Ok(())
}

async fn mcp() -> Result<(), Fail> {
	let s = settings(None, None)?;
	let Some(token) = s.token else {
		return Err(Fail::usage(
			"podkit mcp: set PODKIT_TOKEN to an API token, or run podkit login",
		));
	};
	let stdin = BufReader::new(tokio::io::stdin());
	relay(
		stdin,
		tokio::io::stdout(),
		&cli::client(),
		&Target::new(&s.url, &token),
	)
	.await
	.map_err(|e| Fail::new(format!("podkit mcp: {e}")))
}

async fn run_import(args: Vec<String>) -> Result<(), Fail> {
	if args.iter().any(|a| a == "--help" || a == "-h") {
		print!("{}", import::USAGE);
		return Ok(());
	}
	let parsed = import::parse(args, env)
		.map_err(|e| Fail::usage(format!("podkit import: {e}\n\n{}", import::USAGE)))?;
	let s = settings(None, None)?;
	let Some(token) = s.token else {
		return Err(Fail::usage(
			"podkit import: set PODKIT_TOKEN to an API token, or run podkit login",
		));
	};
	let answer = import::run(&cli::client(), &s.url, &token, &parsed)
		.await
		.map_err(|e| Fail::new(format!("podkit import: {e}")))?;
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
		Ok(())
	} else {
		Err(Fail::new("podkit import: the import did not go through"))
	}
}
