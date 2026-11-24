//! What each command does, over the JSON API. Picking the team, project and application
//! from a slug or an id happens here, as do the pure choices the commands make (which
//! deployment a rollback goes back to, whether a deploy is done), which the tests cover.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{IsTerminal as _, Read as _, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::api::{Api, EXIT_DEPLOY_FAILED, Fail, Result};
use crate::args::{Place, Wait};
use crate::config::{self, Config, Settings, Source};
use crate::{dotenv, sse};

/// How often a deploy's status is asked for while it has no log to follow.
const POLL: Duration = Duration::from_secs(2);
/// How often the health gate is asked about a running release during `--settle`.
const SETTLE_POLL: Duration = Duration::from_secs(5);
/// The most lines the server's bulk endpoint takes at once.
const BULK_LINES: usize = 500;

// --- picking things by slug or id ---------------------------------------------------------

/// Whether `item` is the one `wanted` names: its id, or its slug.
fn names(item: &Value, wanted: &str) -> bool {
	let wanted = wanted.trim();
	item["slug"].as_str() == Some(wanted)
		|| item["id"]
			.as_i64()
			.is_some_and(|id| id.to_string() == wanted)
}

/// `slug (id)` of each item, for a message that lists the choices.
fn choices(items: &[Value]) -> String {
	let list: Vec<String> = items
		.iter()
		.map(|i| {
			format!(
				"{} ({})",
				i["slug"].as_str().unwrap_or("?"),
				i["id"].as_i64().unwrap_or_default()
			)
		})
		.collect();
	if list.is_empty() {
		"none".to_owned()
	} else {
		list.join(", ")
	}
}

/// The one item `wanted` names, or the only item when nothing is wanted.
///
/// # Errors
/// When nothing matches, or several could be meant.
pub fn pick<'a>(
	items: &'a [Value],
	wanted: Option<&str>,
	what: &str,
	flag: &str,
) -> Result<&'a Value> {
	match wanted {
		Some(w) => items.iter().find(|i| names(i, w)).ok_or_else(|| {
			Fail::usage(format!(
				"no {what} {w} that this token can see; there is: {}",
				choices(items)
			))
		}),
		None => match items {
			[one] => Ok(one),
			[] => Err(Fail::new(format!("this token sees no {what}"))),
			_ => Err(Fail::usage(format!(
				"there are several; pick the {what} with {flag}: {}",
				choices(items)
			))),
		},
	}
}

fn id_of(v: &Value) -> i64 {
	v["id"].as_i64().unwrap_or_default()
}

fn text(v: &Value, key: &str) -> String {
	v[key].as_str().unwrap_or_default().to_owned()
}

/// An application and where it lives.
#[derive(Debug, Clone)]
pub struct AppRef {
	/// The team's id.
	pub team: i64,
	/// The project's id.
	pub project: i64,
	/// The project's slug, for messages.
	pub project_slug: String,
	/// The application as `GET …/applications` lists it.
	pub app: Value,
}

impl AppRef {
	fn id(&self) -> i64 {
		id_of(&self.app)
	}

	fn slug(&self) -> String {
		text(&self.app, "slug")
	}

	/// `/teams/{t}/projects/{p}/applications/{a}`.
	#[must_use]
	pub fn path(&self) -> String {
		format!(
			"/teams/{}/projects/{}/applications/{}",
			self.team,
			self.project,
			self.id()
		)
	}
}

/// What a command runs with: the API and the merged settings.
pub struct Ctx {
	/// The API, with the token.
	pub api: Api,
	/// What flags, environment and config file said.
	pub settings: Settings,
}

impl Ctx {
	/// A context for commands that need a token.
	///
	/// # Errors
	/// When no token is set anywhere.
	pub fn new(settings: Settings) -> Result<Self> {
		let Some(token) = settings.token.clone() else {
			return Err(Fail::usage(
				"no API token: run podkit login, or set PODKIT_TOKEN to a token (pk_...) made in Settings -> API tokens",
			));
		};
		Ok(Self {
			api: Api::new(crate::client(), &settings.url, &token),
			settings,
		})
	}

	async fn team(&self, place: &Place) -> Result<Value> {
		let teams = self.api.get("/teams").await?;
		let teams = teams.as_array().cloned().unwrap_or_default();
		let wanted = place.team.as_deref().or(self.settings.team.as_deref());
		pick(&teams, wanted, "team", "--team").cloned()
	}

	async fn projects(&self, team: i64) -> Result<Vec<Value>> {
		let list = self.api.get(&format!("/teams/{team}/projects")).await?;
		Ok(list.as_array().cloned().unwrap_or_default())
	}

	async fn apps_of(&self, team: i64, project: i64) -> Result<Vec<Value>> {
		let list = self
			.api
			.get(&format!("/teams/{team}/projects/{project}/applications"))
			.await?;
		Ok(list.as_array().cloned().unwrap_or_default())
	}

	fn wanted_project<'a>(&'a self, place: &'a Place) -> Option<&'a str> {
		place
			.project
			.as_deref()
			.or(self.settings.project.as_deref())
	}

	/// The application `wanted` names. Without a project it is looked for in every
	/// project of the team, and must be in exactly one.
	async fn app(&self, place: &Place, wanted: &str) -> Result<AppRef> {
		let team = id_of(&self.team(place).await?);
		let projects = self.projects(team).await?;
		let searched: Vec<&Value> = match self.wanted_project(place) {
			Some(p) => vec![pick(&projects, Some(p), "project", "--project")?],
			None => projects.iter().collect(),
		};
		let mut found = Vec::new();
		for project in searched {
			let apps = self.apps_of(team, id_of(project)).await?;
			if let Some(app) = apps.into_iter().find(|a| names(a, wanted)) {
				found.push(AppRef {
					team,
					project: id_of(project),
					project_slug: text(project, "slug"),
					app,
				});
			}
		}
		match found.len() {
			1 => Ok(found.remove(0)),
			0 => Err(Fail::usage(format!(
				"no application {wanted} in {}",
				self.wanted_project(place).map_or_else(
					|| "any project this token can see".to_owned(),
					|p| format!("project {p}")
				)
			))),
			_ => Err(Fail::usage(format!(
				"there is an application {wanted} in several projects ({}); pick one with --project",
				found
					.iter()
					.map(|f| f.project_slug.as_str())
					.collect::<Vec<_>>()
					.join(", ")
			))),
		}
	}
}

// --- output helpers -----------------------------------------------------------------------

/// Lays `rows` out in columns, the first row being the header.
#[must_use]
pub fn table(rows: &[Vec<String>]) -> String {
	let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
	let widths: Vec<usize> = (0..columns)
		.map(|c| {
			rows.iter()
				.map(|r| r.get(c).map_or(0, |s| s.chars().count()))
				.max()
				.unwrap_or(0)
		})
		.collect();
	let mut out = String::new();
	for row in rows {
		let mut line = String::new();
		for (c, cell) in row.iter().enumerate() {
			if c + 1 == row.len() {
				line.push_str(cell);
			} else {
				let _ = write!(line, "{cell:<w$}  ", w = widths[c]);
			}
		}
		out.push_str(line.trim_end());
		out.push('\n');
	}
	out
}

fn print_json(v: &Value) {
	println!(
		"{}",
		serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
	);
}

fn short_sha(v: &Value) -> String {
	v["commit_sha"]
		.as_str()
		.map(|s| s.chars().take(7).collect())
		.unwrap_or_default()
}

// --- login, logout, whoami ----------------------------------------------------------------

/// Reads the token from stdin: everything piped in, or one line typed at a prompt
/// (without echo where `stty` can turn it off).
fn read_token() -> Result<String> {
	let stdin = std::io::stdin();
	let mut token = String::new();
	if stdin.is_terminal() {
		eprint!("API token (pk_...): ");
		let _ = std::io::stderr().flush();
		let quiet = stty("-echo");
		let read = stdin.read_line(&mut token);
		if quiet {
			stty("echo");
			eprintln!();
		}
		read.map_err(|e| Fail::new(format!("could not read the token: {e}")))?;
	} else {
		stdin
			.lock()
			.read_to_string(&mut token)
			.map_err(|e| Fail::new(format!("could not read the token from stdin: {e}")))?;
	}
	Ok(token.trim().to_owned())
}

/// Runs `stty <arg>` on the terminal; whether it worked.
fn stty(arg: &str) -> bool {
	if cfg!(not(unix)) {
		return false;
	}
	std::process::Command::new("stty")
		.arg(arg)
		.stdin(std::process::Stdio::inherit())
		.status()
		.is_ok_and(|s| s.success())
}

/// Whether a token sent to `url` would cross the network in the clear.
#[must_use]
pub fn in_the_clear(url: &str) -> bool {
	let Some(rest) = url.strip_prefix("http://") else {
		return false;
	};
	if rest.starts_with("[::1]") {
		return false;
	}
	let host = rest.split(['/', ':']).next().unwrap_or_default();
	host != "localhost" && !host.starts_with("127.")
}

/// `pk_1a2b3c4d_…`: enough to tell tokens apart, nothing that works as one.
#[must_use]
pub fn token_hint(token: &str) -> String {
	match token.strip_prefix("pk_").and_then(|r| r.split_once('_')) {
		Some((prefix, _)) => format!("pk_{prefix}_…"),
		None => "a session token".to_owned(),
	}
}

/// `podkit login`.
///
/// # Errors
/// When the token is missing or refused, or the file cannot be written.
pub async fn login(
	file: &Path,
	env: &Settings,
	url: Option<String>,
	token: Option<String>,
	place: Place,
) -> Result<()> {
	let url = url
		.map(|u| u.trim().trim_end_matches('/').to_owned())
		.filter(|u| !u.is_empty())
		.unwrap_or_else(|| env.url.clone());
	if !url.starts_with("http://") && !url.starts_with("https://") {
		return Err(Fail::usage(format!(
			"{url} is not an address; give it as https://podkit.example.com"
		)));
	}
	let token = match token {
		Some(t) => t.trim().to_owned(),
		None => read_token()?,
	};
	if token.is_empty() {
		return Err(Fail::usage("no token given"));
	}
	if !token.starts_with("pk_") {
		eprintln!(
			"podkit: warning: that is not an API token (pk_...). A session token works until it expires; make an API token in Settings -> API tokens"
		);
	}
	if in_the_clear(&url) {
		eprintln!(
			"podkit: warning: {url} is plain HTTP, so the token crosses the network unencrypted"
		);
	}
	let api = Api::new(crate::client(), &url, &token);
	let me = api.get("/me").await?;
	// The team the token was made in is the natural default.
	let team = match (&place.team, me["token"]["team_id"].as_i64()) {
		(Some(t), _) => {
			let teams = api.get("/teams").await?;
			let teams = teams.as_array().cloned().unwrap_or_default();
			Some(text(pick(&teams, Some(t), "team", "--team")?, "slug"))
		}
		(None, Some(id)) => {
			let teams = api.get("/teams").await?;
			teams
				.as_array()
				.and_then(|t| t.iter().find(|t| id_of(t) == id))
				.map(|t| text(t, "slug"))
		}
		(None, None) => None,
	};
	let saved = Config {
		url: Some(url.clone()),
		token: Some(token),
		team,
		project: place.project,
	};
	config::save(file, &saved).map_err(Fail::new)?;
	println!(
		"Logged in to {url} as {} <{}>; saved to {}",
		text(&me, "name"),
		text(&me, "email"),
		file.display()
	);
	if env.token_from == Source::Env || env.url_from == Source::Env {
		eprintln!("podkit: note: PODKIT_URL or PODKIT_TOKEN is set and wins over the saved values");
	}
	Ok(())
}

/// `podkit logout`.
///
/// # Errors
/// When the file cannot be deleted.
pub fn logout(file: &Path) -> Result<()> {
	if config::remove(file).map_err(Fail::new)? {
		println!(
			"Deleted {}. The token still works until you revoke it in Settings -> API tokens.",
			file.display()
		);
	} else {
		println!("Not logged in ({} does not exist).", file.display());
	}
	Ok(())
}

/// `podkit whoami`.
///
/// # Errors
/// When podkit cannot be reached or refuses the token.
pub async fn whoami(ctx: &Ctx, json: bool) -> Result<()> {
	let me = ctx.api.get("/me").await?;
	let s = &ctx.settings;
	if json {
		let mut out = me;
		out["url"] = json!(s.url);
		out["team"] = json!(s.team);
		out["project"] = json!(s.project);
		print_json(&out);
		return Ok(());
	}
	let mut rows = vec![
		vec![
			"podkit".to_owned(),
			format!("{} (from {})", s.url, s.url_from.label()),
		],
		vec![
			"user".to_owned(),
			format!("{} <{}>", text(&me, "name"), text(&me, "email")),
		],
	];
	let token = s.token.as_deref().unwrap_or_default();
	let token_line = match me.get("token").filter(|t| !t.is_null()) {
		Some(t) => {
			let scopes: Vec<&str> = t["scopes"]
				.as_array()
				.map(|a| a.iter().filter_map(Value::as_str).collect())
				.unwrap_or_default();
			let projects: Vec<String> = t["project_ids"]
				.as_array()
				.map(|a| a.iter().map(ToString::to_string).collect())
				.unwrap_or_default();
			format!(
				"{} (from {}): scopes {}; team {}; projects {}",
				token_hint(token),
				s.token_from.label(),
				scopes.join(", "),
				t["team_id"],
				if projects.is_empty() {
					"all".to_owned()
				} else {
					projects.join(", ")
				}
			)
		}
		None => format!(
			"{} (from {}): everything your role allows",
			token_hint(token),
			s.token_from.label()
		),
	};
	rows.push(vec!["token".to_owned(), token_line]);
	if let Some(t) = &s.team {
		rows.push(vec!["team".to_owned(), t.clone()]);
	}
	if let Some(p) = &s.project {
		rows.push(vec!["project".to_owned(), p.clone()]);
	}
	print!("{}", table(&rows));
	Ok(())
}

// --- apps and deployments -----------------------------------------------------------------

/// `podkit apps`.
///
/// # Errors
/// When podkit cannot be reached or refuses, or the team or project is unknown.
pub async fn apps(ctx: &Ctx, place: &Place, json: bool) -> Result<()> {
	let team = id_of(&ctx.team(place).await?);
	let projects = ctx.projects(team).await?;
	let chosen: Vec<&Value> = match ctx.wanted_project(place) {
		Some(p) => vec![pick(&projects, Some(p), "project", "--project")?],
		None => projects.iter().collect(),
	};
	let mut all = Vec::new();
	for project in chosen {
		for mut app in ctx.apps_of(team, id_of(project)).await? {
			app["project_slug"] = project["slug"].clone();
			all.push(app);
		}
	}
	if json {
		print_json(&Value::Array(all));
		return Ok(());
	}
	if all.is_empty() {
		println!("No applications.");
		return Ok(());
	}
	let mut rows = vec![
		["PROJECT", "APP", "ID", "NAME", "SOURCE", "URL"]
			.map(str::to_owned)
			.to_vec(),
	];
	for a in &all {
		let source = if a["source_kind"].as_str() == Some("image") {
			text(a, "image")
		} else {
			let repo = a["repo_url"]
				.as_str()
				.filter(|r| !r.is_empty())
				.or(a["git_repo"].as_str())
				.unwrap_or_default();
			format!("{repo}@{}", text(a, "git_ref"))
		};
		rows.push(vec![
			text(a, "project_slug"),
			text(a, "slug"),
			id_of(a).to_string(),
			text(a, "name"),
			source,
			text(a, "url"),
		]);
	}
	print!("{}", table(&rows));
	Ok(())
}

/// The health column of a deployment: the verdict and why.
fn health(d: &Value) -> String {
	match (d["health"].as_str(), d["health_detail"].as_str()) {
		(Some(h), Some(detail)) if !detail.is_empty() => format!("{h}: {detail}"),
		(Some(h), _) => h.to_owned(),
		_ => String::new(),
	}
}

/// `podkit deployments`.
///
/// # Errors
/// When podkit cannot be reached or refuses, or the application is unknown.
pub async fn deployments(
	ctx: &Ctx,
	place: &Place,
	app: &str,
	limit: u32,
	json: bool,
) -> Result<()> {
	let app = ctx.app(place, app).await?;
	let list = ctx
		.api
		.get(&format!("{}/deployments?limit={limit}", app.path()))
		.await?;
	if json {
		print_json(&list);
		return Ok(());
	}
	let list = list.as_array().cloned().unwrap_or_default();
	if list.is_empty() {
		println!("{} has no deployments yet.", app.slug());
		return Ok(());
	}
	let mut rows = vec![
		["ID", "STATUS", "COMMIT", "HEALTH", "ERROR"]
			.map(str::to_owned)
			.to_vec(),
	];
	for d in &list {
		rows.push(vec![
			id_of(d).to_string(),
			text(d, "status"),
			short_sha(d),
			health(d),
			text(d, "error_message"),
		]);
	}
	print!("{}", table(&rows));
	Ok(())
}

// --- deploy, rollback, waiting ------------------------------------------------------------

/// Where a deployment stands, for `--wait`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
	/// Queued, building or deploying.
	InFlight,
	/// Up, and healthy if it has a gate.
	Running,
	/// Done badly; the message says how.
	Failed(String),
	/// Stopped because a newer deployment took over, without the gate finding it bad.
	Replaced,
}

/// Reads a deployment's status and health gate verdict.
#[must_use]
pub fn verdict(d: &Value) -> Verdict {
	let detail = || {
		let mut why = Vec::new();
		if let Some(e) = d["error_message"].as_str().filter(|e| !e.is_empty()) {
			why.push(e.to_owned());
		}
		if let Some(h) = d["health_detail"].as_str().filter(|e| !e.is_empty()) {
			why.push(format!("health check: {h}"));
		}
		if why.is_empty() {
			String::new()
		} else {
			format!(": {}", why.join("; "))
		}
	};
	let id = id_of(d);
	if d["health"].as_str() == Some("rolled_back") {
		return Verdict::Failed(format!(
			"deployment {id} went live, failed its health check and was rolled back{}",
			detail()
		));
	}
	match d["status"].as_str().unwrap_or_default() {
		"queued" | "building" | "deploying" => Verdict::InFlight,
		"running" => Verdict::Running,
		"stopped" => Verdict::Replaced,
		"failed" if d["health"].as_str() == Some("unhealthy") => Verdict::Failed(format!(
			"deployment {id} never passed its health check; the previous container kept serving{}",
			detail()
		)),
		"cancelled" => Verdict::Failed(format!("deployment {id} was cancelled")),
		other => Verdict::Failed(format!("deployment {id} {other}{}", detail())),
	}
}

/// The deployment a rollback goes back to when none is named: the newest one older than
/// what runs now that kept its image and was not found bad, as the health gate picks.
#[must_use]
pub fn rollback_target(newest_first: &[Value]) -> Option<&Value> {
	let start = newest_first
		.iter()
		.position(|d| d["status"].as_str() == Some("running"))
		.map_or(0, |i| i + 1);
	newest_first[start..].iter().find(|d| {
		d["status"].as_str() == Some("stopped")
			&& d["image_tag"].as_str().is_some_and(|t| !t.is_empty())
			&& !matches!(d["health"].as_str(), Some("unhealthy" | "rolled_back"))
	})
}

/// Where build output goes: stdout, or stderr when stdout carries JSON.
fn out(to_stderr: bool, line: &str) {
	if to_stderr {
		eprintln!("{line}");
	} else {
		println!("{line}");
	}
}

/// How one follow of the build log ended.
struct Followed {
	/// Lines printed.
	lines: usize,
	/// The server said the log is over (rather than the connection dropping).
	ended: bool,
}

/// Prints the build log from after `last_seq` until the server ends it or `deadline`.
async fn follow_build(
	ctx: &Ctx,
	path: &str,
	last_seq: &mut u64,
	deadline: Instant,
	to_stderr: bool,
) -> Result<Followed> {
	let mut response = ctx.api.events(&format!("{path}/build-logs")).await?;
	let mut parser = sse::Parser::default();
	let mut got = Followed {
		lines: 0,
		ended: false,
	};
	loop {
		let left = deadline.saturating_duration_since(Instant::now());
		let chunk = match tokio::time::timeout(left, response.chunk()).await {
			Err(_) | Ok(Err(_) | Ok(None)) => return Ok(got),
			Ok(Ok(Some(chunk))) => chunk,
		};
		for event in parser.feed(&chunk) {
			match event.name.as_str() {
				"log" => {
					let Ok(line) = serde_json::from_str::<Value>(&event.data) else {
						continue;
					};
					let seq = line["seq"].as_u64().unwrap_or_default();
					if seq <= *last_seq {
						continue;
					}
					*last_seq = seq;
					got.lines += 1;
					let textline = line["line"].as_str().unwrap_or_default();
					match line["stream"].as_str() {
						Some("stderr") => eprintln!("{textline}"),
						Some("system") => out(to_stderr, &format!("==> {textline}")),
						_ => out(to_stderr, textline),
					}
				}
				"lagged" => eprintln!(
					"podkit: {} log lines were skipped (the build wrote faster than they could be sent)",
					event.data
				),
				"end" => {
					got.ended = true;
					return Ok(got);
				}
				_ => {}
			}
		}
	}
}

/// Follows deployment `id` until it is running (and, with `settle`, stays so), and returns
/// it as podkit last reported it.
async fn wait_for(ctx: &Ctx, app: &AppRef, id: i64, wait: Wait, to_stderr: bool) -> Result<Value> {
	let path = format!("{}/deployments/{id}", app.path());
	let deadline = Instant::now() + Duration::from_secs(wait.timeout);
	let timed_out = || {
		Fail::new(format!(
			"gave up waiting after {}s; the deploy goes on: podkit deployments {}",
			wait.timeout,
			app.slug()
		))
	};
	let mut last_seq = 0;
	let mut follow = true;
	let d = loop {
		if follow {
			let got = follow_build(ctx, &path, &mut last_seq, deadline, to_stderr).await?;
			// Once a log was read to its end, only the status is left to wait for. A log
			// that had not started yet, or a dropped connection, is followed again.
			follow = !(got.ended && last_seq > 0);
		}
		let d = ctx.api.get(&path).await?;
		match verdict(&d) {
			Verdict::InFlight => {}
			Verdict::Running | Verdict::Replaced => break d,
			Verdict::Failed(why) => {
				return Err(Fail {
					code: EXIT_DEPLOY_FAILED,
					message: why,
				});
			}
		}
		if Instant::now() >= deadline {
			return Err(timed_out());
		}
		tokio::time::sleep(POLL).await;
	};
	if wait.settle == 0 || verdict(&d) == Verdict::Replaced {
		return Ok(d);
	}
	eprintln!(
		"podkit: deployment {id} is running; watching the health gate for {}s",
		wait.settle
	);
	let until = Instant::now() + Duration::from_secs(wait.settle);
	let mut d = d;
	while Instant::now() < until {
		tokio::time::sleep(SETTLE_POLL.min(until.saturating_duration_since(Instant::now()))).await;
		d = ctx.api.get(&path).await?;
		match verdict(&d) {
			Verdict::Failed(why) => {
				return Err(Fail {
					code: EXIT_DEPLOY_FAILED,
					message: why,
				});
			}
			Verdict::Replaced => {
				eprintln!("podkit: deployment {id} was replaced by a newer one");
				break;
			}
			Verdict::InFlight | Verdict::Running => {}
		}
	}
	Ok(d)
}

/// Prints the outcome of a deploy or rollback and, with `--wait`, waits for it.
async fn started(
	ctx: &Ctx,
	app: &AppRef,
	d: &Value,
	wait: Wait,
	json: bool,
	what: &str,
) -> Result<()> {
	let id = id_of(d);
	let say = |line: &str| out(json, line);
	say(&format!(
		"{what} {} ({}/{}), deployment {id}",
		app.slug(),
		app.project_slug,
		app.id()
	));
	if !wait.on {
		if json {
			print_json(d);
		} else {
			println!("Watch it with: podkit deployments {}", app.slug());
		}
		return Ok(());
	}
	let done = wait_for(ctx, app, id, wait, json).await?;
	if json {
		print_json(&done);
	} else {
		let url = text(&app.app, "url");
		let at = if url.is_empty() {
			String::new()
		} else {
			format!(" at {url}")
		};
		println!("Deployment {id} is {}{at}", text(&done, "status"));
	}
	Ok(())
}

/// `podkit deploy`.
///
/// # Errors
/// When podkit refuses, or with `--wait` when the deploy does not end up running.
pub async fn deploy(ctx: &Ctx, place: &Place, app: &str, wait: Wait, json: bool) -> Result<()> {
	let app = ctx.app(place, app).await?;
	let d = ctx
		.api
		.post(&format!("{}/deployments", app.path()), None)
		.await?;
	started(ctx, &app, &d, wait, json, "Deploying").await
}

/// `podkit rollback`.
///
/// # Errors
/// When there is nothing to go back to, podkit refuses, or with `--wait` when the
/// rollback does not end up running.
pub async fn rollback(
	ctx: &Ctx,
	place: &Place,
	app: &str,
	to: Option<i64>,
	wait: Wait,
	json: bool,
) -> Result<()> {
	let app = ctx.app(place, app).await?;
	let target = if let Some(id) = to {
		id
	} else {
		let list = ctx
			.api
			.get(&format!("{}/deployments?limit=200", app.path()))
			.await?;
		let list = list.as_array().cloned().unwrap_or_default();
		let target = rollback_target(&list).ok_or_else(|| {
			Fail::new(format!(
				"{} has no earlier deployment with an image to go back to; see podkit deployments {}",
				app.slug(),
				app.slug()
			))
		})?;
		let sha = short_sha(target);
		out(
			json,
			&format!(
				"Going back to deployment {}{}",
				id_of(target),
				if sha.is_empty() {
					String::new()
				} else {
					format!(" (commit {sha})")
				}
			),
		);
		id_of(target)
	};
	let d = ctx
		.api
		.post(
			&format!("{}/deployments/{target}/rollback", app.path()),
			None,
		)
		.await?;
	started(ctx, &app, &d, wait, json, "Rolling back").await
}

// --- logs ---------------------------------------------------------------------------------

/// `podkit logs`.
///
/// # Errors
/// When podkit refuses (no running container, missing scope) or cannot be reached.
pub async fn logs(ctx: &Ctx, place: &Place, app: &str, follow: bool, tail: u32) -> Result<()> {
	let app = ctx.app(place, app).await?;
	let mut response = ctx
		.api
		.events(&format!("{}/logs?tail={tail}&follow={follow}", app.path()))
		.await?;
	let mut parser = sse::Parser::default();
	let stdout = std::io::stdout();
	loop {
		let chunk = match response.chunk().await {
			Ok(Some(c)) => c,
			Ok(None) => return Ok(()),
			Err(e) => return Err(Fail::new(format!("the log stream broke off: {e}"))),
		};
		for event in parser.feed(&chunk) {
			match event.name.as_str() {
				"log" => {
					let Ok(line) = serde_json::from_str::<Value>(&event.data) else {
						continue;
					};
					let textline = line["line"].as_str().unwrap_or_default();
					if line["stream"].as_str() == Some("stderr") {
						eprintln!("{textline}");
					} else {
						let mut lock = stdout.lock();
						// A closed pipe (`| head`) ends the command quietly.
						if writeln!(lock, "{textline}").is_err() {
							return Ok(());
						}
					}
				}
				"end" => return Ok(()),
				_ => {}
			}
		}
	}
}

// --- env pull and push --------------------------------------------------------------------

/// `podkit env pull`.
///
/// # Errors
/// When the file exists (without `--force`), cannot be written, or podkit refuses.
pub async fn env_pull(ctx: &Ctx, place: &Place, app: &str, file: &str, force: bool) -> Result<()> {
	let to_stdout = file == "-";
	if !to_stdout && !force && Path::new(file).exists() {
		return Err(Fail::usage(format!(
			"{file} already exists; pass --force to overwrite it"
		)));
	}
	let app = ctx.app(place, app).await?;
	let list = ctx.api.get(&format!("{}/env-vars", app.path())).await?;
	let list = list.as_array().cloned().unwrap_or_default();
	// podkit never hands values out today; should a future one do so for a token that
	// may read secrets, they are written.
	let vars: Vec<(String, Option<String>)> = list
		.iter()
		.map(|v| (text(v, "key"), v["value"].as_str().map(str::to_owned)))
		.collect();
	let with_values = vars.iter().filter(|(_, v)| v.is_some()).count();
	let origin = format!(
		"podkit env of {}/{} from {}",
		app.project_slug,
		app.slug(),
		ctx.settings.url
	);
	let mut header = vec![origin.as_str()];
	if with_values < vars.len() {
		header.push("Values are write-only in podkit, so the names below have none. Fill them in;");
		header.push("podkit env push skips a variable left empty here, so nothing is blanked.");
	}
	let rendered = dotenv::render(
		&header,
		vars.iter().map(|(k, v)| (k.as_str(), v.as_deref())),
	)
	.map_err(Fail::new)?;
	if to_stdout {
		print!("{rendered}");
	} else {
		config::write_private(Path::new(file), rendered.as_bytes())
			.map_err(|e| Fail::new(format!("could not write {file}: {e}")))?;
		eprintln!(
			"Wrote {} variables of {} to {file}{}",
			vars.len(),
			app.slug(),
			if with_values < vars.len() {
				" (names only: values are write-only)"
			} else {
				""
			}
		);
	}
	Ok(())
}

/// Asks a yes/no question on the terminal; `false` unless the answer starts with y.
fn confirm(question: &str) -> Result<bool> {
	if !std::io::stdin().is_terminal() {
		return Err(Fail::usage(
			"stdin is not a terminal, so there is nobody to ask: pass --yes to go ahead",
		));
	}
	eprint!("{question} [y/N] ");
	let _ = std::io::stderr().flush();
	let mut answer = String::new();
	std::io::stdin()
		.read_line(&mut answer)
		.map_err(|e| Fail::new(format!("could not read the answer: {e}")))?;
	Ok(answer.trim().to_ascii_lowercase().starts_with('y'))
}

/// `podkit env push` and its options.
#[allow(
	clippy::struct_excessive_bools,
	reason = "they are the command's switches"
)]
pub struct Push<'a> {
	/// The `.env` file; `-` is stdin.
	pub file: &'a str,
	/// Do not ask before changing anything.
	pub yes: bool,
	/// Delete the variables the file does not have.
	pub prune: bool,
	/// Set variables the file leaves empty to an empty value.
	pub allow_empty: bool,
	/// Print the changes and stop.
	pub dry_run: bool,
}

/// `podkit env push`.
///
/// # Errors
/// When the file has bad lines or cannot be read, the answer is no, or podkit refuses.
pub async fn env_push(ctx: &Ctx, place: &Place, app: &str, push: Push<'_>) -> Result<()> {
	let from_stdin = push.file == "-";
	if from_stdin && !push.yes && !push.dry_run {
		return Err(Fail::usage(
			"the file comes from stdin, so there is nobody to ask: pass --yes",
		));
	}
	let content = if from_stdin {
		let mut s = String::new();
		std::io::stdin()
			.read_to_string(&mut s)
			.map_err(|e| Fail::new(format!("could not read stdin: {e}")))?;
		s
	} else {
		std::fs::read_to_string(push.file)
			.map_err(|e| Fail::usage(format!("could not read {}: {e}", push.file)))?
	};
	let (entries, errors) = dotenv::parse(&content);
	if !errors.is_empty() {
		let mut msg = format!("{} has lines podkit would skip; fix them first:", push.file);
		for e in &errors {
			let _ = write!(msg, "\n  line {}: {}", e.line, e.message);
		}
		return Err(Fail::usage(msg));
	}
	let file = dotenv::by_key(&entries);
	let app = ctx.app(place, app).await?;
	let base = format!("{}/env-vars", app.path());
	let remote = ctx.api.get(&base).await?;
	let remote: BTreeMap<String, i64> = remote
		.as_array()
		.map(|a| a.iter().map(|v| (text(v, "key"), id_of(v))).collect())
		.unwrap_or_default();
	let names: BTreeSet<String> = remote.keys().cloned().collect();
	let diff = dotenv::Diff::of(&file, &names, push.allow_empty);

	println!("{} ({}):", app.slug(), app.project_slug);
	print!("{}", diff.summary(push.prune));
	if diff.is_empty(push.prune) {
		println!("Nothing to change.");
		return Ok(());
	}
	if push.dry_run {
		return Ok(());
	}
	if !push.yes && !confirm(&format!("Push these changes to {}?", app.slug()))? {
		return Err(Fail::new("nothing was changed"));
	}

	let send = diff.to_send(&file);
	let mut saved = 0;
	for chunk in send.chunks(BULK_LINES) {
		let text =
			dotenv::render(&[], chunk.iter().map(|(k, v)| (*k, Some(*v)))).map_err(Fail::new)?;
		let answer = ctx
			.api
			.post(&format!("{base}/bulk"), Some(&json!({ "text": text })))
			.await?;
		saved += answer["saved"].as_u64().unwrap_or_default();
		if let Some(errors) = answer["errors"].as_array().filter(|e| !e.is_empty()) {
			return Err(Fail::new(format!(
				"podkit skipped {} of the lines sent: {}",
				errors.len(),
				Value::Array(errors.clone())
			)));
		}
	}
	let mut deleted = 0;
	if push.prune {
		for key in &diff.missing {
			if let Some(id) = remote.get(key) {
				ctx.api.delete(&format!("{base}/{id}")).await?;
				deleted += 1;
			}
		}
	}
	println!(
		"Set {saved} variables{}. They take effect on the next deploy: podkit deploy {}",
		if push.prune {
			format!(", deleted {deleted}")
		} else {
			String::new()
		},
		app.slug()
	);
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn items() -> Vec<Value> {
		vec![
			json!({ "id": 11, "slug": "chikof" }),
			json!({ "id": 12, "slug": "other" }),
		]
	}

	#[test]
	fn pick_by_slug_id_or_the_only_one() {
		let all = items();
		assert_eq!(
			id_of(pick(&all, Some("other"), "team", "--team").unwrap()),
			12
		);
		assert_eq!(
			id_of(pick(&all, Some(" 11 "), "team", "--team").unwrap()),
			11
		);
		let err = pick(&all, Some("nope"), "team", "--team").unwrap_err();
		assert_eq!(err.code, crate::api::EXIT_USAGE);
		assert!(
			err.message.contains("chikof (11), other (12)"),
			"{}",
			err.message
		);
		let err = pick(&all, None, "team", "--team").unwrap_err();
		assert!(err.message.contains("--team"));
		assert_eq!(id_of(pick(&all[..1], None, "team", "--team").unwrap()), 11);
		assert!(pick(&[], None, "team", "--team").is_err());
	}

	#[test]
	fn verdicts_cover_status_and_the_health_gate() {
		let d = |status: &str, health: Value| json!({ "id": 5, "status": status, "health": health, "error_message": null });
		assert_eq!(verdict(&d("building", Value::Null)), Verdict::InFlight);
		assert_eq!(
			verdict(&d("deploying", json!("waiting"))),
			Verdict::InFlight
		);
		assert_eq!(verdict(&d("running", json!("healthy"))), Verdict::Running);
		assert_eq!(verdict(&d("running", Value::Null)), Verdict::Running);
		assert_eq!(verdict(&d("stopped", Value::Null)), Verdict::Replaced);
		assert!(
			matches!(verdict(&d("cancelled", Value::Null)), Verdict::Failed(m) if m.contains("cancelled"))
		);
		assert!(
			matches!(verdict(&d("crashed", Value::Null)), Verdict::Failed(m) if m.contains("crashed"))
		);
		let mut unhealthy = d("failed", json!("unhealthy"));
		unhealthy["health_detail"] = json!("GET / gave 502");
		assert!(
			matches!(verdict(&unhealthy), Verdict::Failed(m) if m.contains("never passed") && m.contains("502"))
		);
		let mut failed = d("failed", Value::Null);
		failed["error_message"] = json!("build exited 1");
		assert!(matches!(verdict(&failed), Verdict::Failed(m) if m.contains("build exited 1")));
		// Rolled back wins over whatever the status says.
		assert!(
			matches!(verdict(&d("stopped", json!("rolled_back"))), Verdict::Failed(m) if m.contains("rolled back"))
		);
	}

	#[test]
	fn rollback_goes_to_the_newest_good_image_before_the_running_one() {
		let d = |id: i64, status: &str, image: Option<&str>, health: Option<&str>| json!({ "id": id, "status": status, "image_tag": image, "health": health });
		let list = vec![
			d(9, "failed", None, None),
			d(8, "running", Some("i8"), Some("healthy")),
			d(7, "stopped", Some("i7"), Some("rolled_back")),
			d(6, "failed", Some("i6"), Some("unhealthy")),
			d(5, "stopped", None, None),
			d(4, "stopped", Some("i4"), None),
			d(3, "stopped", Some("i3"), None),
		];
		assert_eq!(rollback_target(&list).map(id_of), Some(4));
		// Nothing running: the newest good one at all.
		let idle = vec![
			d(2, "stopped", Some("i2"), None),
			d(1, "stopped", Some("i1"), None),
		];
		assert_eq!(rollback_target(&idle).map(id_of), Some(2));
		assert_eq!(rollback_target(&list[..3]), None);
		assert_eq!(rollback_target(&[]), None);
	}

	#[test]
	fn tables_pad_every_column_but_the_last() {
		let rows = vec![
			vec!["A".to_owned(), "LONG HEADER".to_owned(), "X".to_owned()],
			vec!["long cell".to_owned(), "b".to_owned(), String::new()],
		];
		assert_eq!(table(&rows), "A          LONG HEADER  X\nlong cell  b\n");
	}

	#[test]
	fn token_hints_and_plain_http() {
		assert_eq!(token_hint("pk_1a2b3c4d_s3cr3t"), "pk_1a2b3c4d_…");
		assert!(!token_hint("pk_1a2b3c4d_s3cr3t").contains("s3cr3t"));
		assert_eq!(token_hint("abcdef"), "a session token");
		assert!(in_the_clear("http://podkit.example.com"));
		assert!(in_the_clear("http://10.0.0.5:8080/"));
		assert!(!in_the_clear("http://127.0.0.1:8080"));
		assert!(!in_the_clear("http://localhost:8080"));
		assert!(!in_the_clear("http://[::1]:8080"));
		assert!(!in_the_clear("https://podkit.example.com"));
	}
}
