//! `podkit import coolify|dokploy|caprover`: read another platform and plan (or make) its
//! projects in a podkit team.
//!
//! This holds no mapping of its own. It calls podkit's importer API,
//! `POST {PODKIT_URL}/api/v1/teams/{team}/importers/{source}/plan|apply`, with the podkit
//! API token, and prints what the server answers: what was read, what did not map, and the
//! plan. The source's credential goes to podkit in the request body for that one request;
//! the server never stores it. Nothing is made without `--apply`.

use std::fmt::Write as _;

use serde_json::{Value, json};

/// The platforms, as the server's route names them.
pub const SOURCES: [&str; 3] = ["coolify", "dokploy", "caprover"];

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
	/// `coolify`, `dokploy` or `caprover`.
	pub source: String,
	/// The source's address.
	pub url: String,
	/// The source's credential: an API token or key, or the `CapRover` password.
	pub token: String,
	/// The podkit server everything is put on; empty is the team's only server.
	pub server: String,
	/// The podkit team to import into.
	pub team: String,
	/// Make what the plan plans.
	pub apply: bool,
	/// Also deploy what is made.
	pub deploy: bool,
	/// Print the answer as JSON instead of text.
	pub json: bool,
}

/// The help text of `podkit import`.
pub const USAGE: &str = "podkit import: bring projects over from Coolify, Dokploy or CapRover

Usage: podkit import <coolify|dokploy|caprover> --url <address> [--server <name>]
                     [--team <id>] [--apply] [--deploy] [--json]

Without --apply it only reads the source and prints the report of what did not map,
and the plan against your team. Nothing is made.

  --url      the source's address, e.g. https://coolify.example.com
  --server   the podkit server to put everything on (default: the team's only server)
  --team     the podkit team id (default: $PODKIT_TEAM)
  --apply    make the projects (with --deploy, also deploy them)
  --json     print the server's answer as JSON

Environment:
  PODKIT_URL     podkit's address (default http://127.0.0.1:8080)
  PODKIT_TOKEN   a podkit API token (pk_...), made in Settings -> API tokens
  PODKIT_TEAM    the team id to import into
  IMPORT_TOKEN   the source's credential (Coolify API token, Dokploy API key, CapRover
                 password). It is read from the environment so it stays out of argv.
";

/// Parses what follows `podkit import`, with the environment as `env`.
///
/// # Errors
/// A message naming what is missing or unknown.
pub fn parse(
	args: impl IntoIterator<Item = String>,
	env: impl Fn(&str) -> Option<String>,
) -> Result<Args, String> {
	let mut it = args.into_iter();
	let source = it.next().ok_or("name the platform to import from")?;
	if !SOURCES.contains(&source.as_str()) {
		return Err(format!(
			"unknown platform {source}; use coolify, dokploy or caprover"
		));
	}
	let mut out = Args {
		source,
		url: String::new(),
		token: env("IMPORT_TOKEN").unwrap_or_default(),
		server: String::new(),
		team: env("PODKIT_TEAM").unwrap_or_default(),
		apply: false,
		deploy: false,
		json: false,
	};
	while let Some(arg) = it.next() {
		let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
		match arg.as_str() {
			"--url" => out.url = value("--url")?,
			"--server" => out.server = value("--server")?,
			"--team" => out.team = value("--team")?,
			"--apply" => out.apply = true,
			"--deploy" => out.deploy = true,
			"--json" => out.json = true,
			other => return Err(format!("unknown option {other}")),
		}
	}
	if out.url.trim().is_empty() {
		return Err("give the source's address with --url".to_owned());
	}
	if out.token.is_empty() {
		return Err("set IMPORT_TOKEN to the source's credential".to_owned());
	}
	if out.team.trim().is_empty() || !out.team.chars().all(|c| c.is_ascii_digit()) {
		return Err("give the podkit team id with --team or PODKIT_TEAM".to_owned());
	}
	if out.deploy && !out.apply {
		return Err("--deploy only means something with --apply".to_owned());
	}
	Ok(out)
}

/// What the server answered.
pub struct Answer {
	/// The HTTP status.
	pub status: u16,
	/// The JSON body.
	pub body: Value,
}

/// The team's only server, for when `--server` is left out.
async fn only_server(
	client: &reqwest::Client,
	base: &str,
	podkit_token: &str,
	team: &str,
) -> Result<String, String> {
	let url = format!("{}/api/v1/teams/{team}/servers", base.trim_end_matches('/'));
	let reply = client
		.get(&url)
		.bearer_auth(podkit_token)
		.send()
		.await
		.map_err(|e| format!("could not reach podkit at {url}: {e}"))?;
	let status = reply.status();
	let list: Value = reply
		.text()
		.await
		.ok()
		.and_then(|t| serde_json::from_str(&t).ok())
		.ok_or_else(|| format!("podkit answered {status} to the list of servers"))?;
	let names: Vec<&str> = list
		.as_array()
		.map_or(&[][..], Vec::as_slice)
		.iter()
		.filter_map(|s| s["name"].as_str())
		.collect();
	match names.as_slice() {
		[one] => Ok((*one).to_owned()),
		[] => Err("this team has no server to put the applications on".to_owned()),
		many => Err(format!(
			"choose a server with --server: {}",
			many.join(", ")
		)),
	}
}

/// Asks podkit to read the source: a plan, or with `--apply` the plan and the apply in one
/// request.
///
/// # Errors
/// When podkit cannot be reached or its answer is not JSON.
pub async fn run(
	client: &reqwest::Client,
	base: &str,
	podkit_token: &str,
	args: &Args,
) -> Result<Answer, String> {
	let verb = if args.apply { "apply" } else { "plan" };
	let url = format!(
		"{}/api/v1/teams/{}/importers/{}/{verb}",
		base.trim_end_matches('/'),
		args.team,
		args.source
	);
	let mut body = json!({ "url": args.url, "token": args.token, "server": args.server });
	if args.server.is_empty() {
		body["server"] = json!(only_server(client, base, podkit_token, &args.team).await?);
	}
	if args.apply && args.deploy {
		body["deploy"] = json!(true);
	}
	let response = client
		.post(&url)
		.bearer_auth(podkit_token)
		.header("content-type", "application/json")
		.body(body.to_string())
		.send()
		.await
		.map_err(|e| format!("could not reach podkit at {url}: {e}"))?;
	let status = response.status().as_u16();
	let text = response
		.text()
		.await
		.map_err(|e| format!("podkit's answer was cut off: {e}"))?;
	let body = serde_json::from_str(&text)
		.map_err(|_| format!("podkit answered {status} with something that is not JSON"))?;
	Ok(Answer { status, body })
}

fn text(v: &Value) -> &str {
	v.as_str().unwrap_or_default()
}

/// The answer as text for a terminal: the report lines, then the plan one change per line.
#[must_use]
pub fn render(source: &str, answer: &Answer) -> String {
	let b = &answer.body;
	let mut out = String::new();
	if let Some(error) = b["error"].as_str() {
		let _ = writeln!(out, "error: {error}");
	}
	let notes = b["report"]["notes"]
		.as_array()
		.map_or(&[][..], Vec::as_slice);
	let counts = &b["report"]["counts"];
	if counts.is_object() {
		let _ = writeln!(
			out,
			"Read {} project(s), {} app(s), {} database(s), {} stack(s), {} variable(s), {} domain(s), {} volume(s).",
			counts["projects"],
			counts["apps"],
			counts["databases"],
			counts["stacks"],
			counts["variables"],
			counts["domains"],
			counts["volumes"],
		);
	}
	if !notes.is_empty() {
		let _ = writeln!(out, "\nNot carried over:");
		for n in notes {
			let label = match text(&n["level"]) {
				"unmapped" => "not supported",
				"skipped" => "skipped",
				_ => "check",
			};
			let at = text(&n["at"]);
			if at.is_empty() {
				let _ = writeln!(out, "  {source}: {} [{label}]", text(&n["message"]));
			} else {
				let _ = writeln!(out, "  {source}: {} [{label}] ({at})", text(&n["message"]));
			}
		}
	}
	for p in b["projects"].as_array().map_or(&[][..], Vec::as_slice) {
		let _ = writeln!(out, "\nProject {} ({})", text(&p["name"]), text(&p["slug"]));
		let plan = if p["plan"].is_object() {
			&p["plan"]
		} else {
			&p["applied"]["plan"]
		};
		for c in plan["changes"].as_array().map_or(&[][..], Vec::as_slice) {
			let env = c["environment"]
				.as_str()
				.map(|e| format!("{e}/"))
				.unwrap_or_default();
			let _ = writeln!(
				out,
				"  {} {} {env}{}",
				text(&c["action"]),
				text(&c["kind"]),
				text(&c["name"])
			);
		}
		for i in plan["issues"].as_array().map_or(&[][..], Vec::as_slice) {
			let _ = writeln!(out, "  issue: {}", text(&i["message"]));
		}
	}
	if let Some(applied) = b["applied"].as_array() {
		let _ = writeln!(out, "\nApplied {} project(s).", applied.len());
	} else if answer.status == 200 {
		let _ = writeln!(
			out,
			"\nNothing was made. Run again with --apply to make it."
		);
	}
	out
}

/// The process exit code for an answer: 0 for success, 1 for anything that stopped the
/// import.
#[must_use]
pub fn ok(answer: &Answer) -> bool {
	answer.status == 200
}

#[cfg(test)]
mod tests {
	use super::*;

	fn args(list: &[&str]) -> Result<Args, String> {
		parse(list.iter().map(|s| (*s).to_owned()), |k| match k {
			"IMPORT_TOKEN" => Some("secret".to_owned()),
			"PODKIT_TEAM" => Some("7".to_owned()),
			_ => None,
		})
	}

	#[test]
	fn importers_cli_parses_a_plan_and_an_apply() {
		let a = args(&["coolify", "--url", "https://c.example"]).unwrap();
		assert_eq!((a.source.as_str(), a.team.as_str()), ("coolify", "7"));
		assert!(!a.apply);
		let a = args(&[
			"caprover", "--url", "x", "--apply", "--deploy", "--team", "9",
		])
		.unwrap();
		assert!(a.apply && a.deploy && a.team == "9");
	}

	#[test]
	fn importers_cli_refuses_what_is_missing_or_unknown() {
		assert!(args(&[]).is_err());
		assert!(
			args(&["heroku", "--url", "x"])
				.unwrap_err()
				.contains("unknown platform")
		);
		assert!(args(&["coolify"]).unwrap_err().contains("--url"));
		assert!(args(&["coolify", "--url", "x", "--deploy"]).is_err());
		assert!(args(&["coolify", "--url", "x", "--bogus"]).is_err());
		assert!(args(&["coolify", "--url", "x", "--team", "a/b"]).is_err());
		assert!(
			parse(["coolify", "--url", "x"].map(str::to_owned), |k| (k
				== "PODKIT_TEAM")
				.then(|| "1".to_owned()))
			.unwrap_err()
			.contains("IMPORT_TOKEN")
		);
	}

	#[test]
	fn importers_cli_renders_the_report_and_plan_without_a_credential() {
		let answer = Answer {
			status: 200,
			body: json!({
				"report": {
					"counts": {"projects": 1, "apps": 1, "databases": 0, "stacks": 0,
						"variables": 2, "domains": 1, "volumes": 0},
					"notes": [{"level": "unmapped", "at": "shop/production/web",
						"message": "custom labels are not supported"}],
				},
				"projects": [{"name": "Shop", "slug": "shop", "plan": {
					"changes": [{"action": "create", "kind": "application",
						"environment": "production", "name": "web"}],
					"issues": [],
				}}],
			}),
		};
		let out = render("Coolify", &answer);
		assert!(out.contains(
			"Coolify: custom labels are not supported [not supported] (shop/production/web)"
		));
		assert!(out.contains("create application production/web"));
		assert!(out.contains("Nothing was made"));
		assert!(!out.contains("secret"));
	}
}
