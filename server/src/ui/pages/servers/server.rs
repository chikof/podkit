//! Server pages: the team's servers (add one), and a server's detail with the
//! live bootstrap log, re-run, host key reset and delete.
//!
//! The SSH private key is write-only: it travels from the form to the create
//! action and is never rendered back, into HTML or into a signal.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use crate::auth::permission::{Action, Resource};
use crate::error::ServerError;
use crate::ids::{ServerId, TeamId};
use crate::jobs::{Job, JobStatus};
use crate::servers::service::{self, DEFAULT_PODMAN_SOCKET_PATH, DEFAULT_SSH_PORT, NewServer};
use crate::servers::{Server, host_key_fingerprint};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, BUTTON_DANGER, BUTTON_OUTLINE, INPUT, LABEL, Tone, card, card_content, card_header,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page, form_error};
use crate::ui::live::{job_log, log_view};
use crate::ui::status::{Status, poll, status_badge};
use crate::ui::{error, sse};

const LOG_ID: &str = "bootstrap-log";

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/servers", get(list_page).post(create))
		.route("/teams/{team_id}/servers/rows", get(rows))
		.route("/teams/{team_id}/servers/{server_id}", get(detail))
		.route("/teams/{team_id}/servers/{server_id}/status", get(status))
		.route(
			"/teams/{team_id}/servers/{server_id}/log",
			get(bootstrap_log),
		)
		.route(
			"/teams/{team_id}/servers/{server_id}/bootstrap",
			post(bootstrap),
		)
		.route("/teams/{team_id}/servers/{server_id}/delete", post(remove))
}

fn list_url(team: TeamId) -> String {
	format!("/teams/{team}/servers")
}

fn detail_url(team: TeamId, server: ServerId) -> String {
	format!("/teams/{team}/servers/{server}")
}

fn crumbs<'a>(ctx: &TeamCtx) -> Vec<Crumb<'a>> {
	vec![("Servers", Some(list_url(ctx.team.id)))]
}

fn job_label(status: JobStatus) -> &'static str {
	match status {
		JobStatus::Queued => "queued",
		JobStatus::Running => "running",
		JobStatus::Succeeded => "finished",
		JobStatus::Failed => "failed",
		JobStatus::Cancelled => "cancelled",
	}
}

fn job_in_flight(job: Option<&Job>) -> bool {
	job.is_some_and(|j| matches!(j.status, JobStatus::Queued | JobStatus::Running))
}

fn host_port(server: &Server) -> String {
	if server.is_local {
		"this host".to_owned()
	} else {
		format!("{}:{}", server.hostname, server.ssh_port)
	}
}

fn local_badge() -> Markup {
	html! {
		span class=(Tone::Neutral.badge_class()) { "Local" }
	}
}

/// The rows of the server list. Re-fetched every few seconds while any
/// server is pending, so the badge turns by itself.
fn server_list(ctx: &TeamCtx, servers: &[Server]) -> Markup {
	let polling = servers.iter().any(|s| s.status.in_flight());
	let rows_url = format!("{}/rows", list_url(ctx.team.id));
	html! {
		div id="server-list" {
			@if servers.is_empty() {
				p class="text-sm text-muted-foreground" { "No servers yet." }
			} @else {
				// Not `divide-y`: encre-css 0.21.1 puts that border below each row.
				ul {
					@for server in servers {
						li class="flex flex-col gap-2 border-t border-border py-3 text-sm first:border-t-0" {
							div class="flex flex-wrap items-center gap-2" {
								a class="font-medium hover:underline" href=(detail_url(ctx.team.id, server.id)) { (server.name) }
								@if server.is_local { (local_badge()) }
								(status_badge(server.status))
								span class="text-muted-foreground" { (host_port(server)) }
							}
							@if let Some(message) = &server.status_message {
								p class="text-destructive" { (message) }
							}
							(facts_line(server))
						}
					}
				}
			}
			(poll(&rows_url, polling))
		}
	}
}

/// Agent version, architecture and host key on one line, when known.
fn facts_line(server: &Server) -> Markup {
	let fingerprint = server.host_key.as_deref().and_then(host_key_fingerprint);
	html! {
		div class="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground" {
			@if let Some(version) = &server.agent_version { span { "Agent " (version) } }
			@if let Some(arch) = &server.arch { span { (arch) } }
			@if let Some(fp) = &fingerprint { span class="font-mono break-all" { (fp) } }
		}
	}
}

fn create_card(ctx: &TeamCtx) -> Markup {
	let post = format!("evt.preventDefault(); @post('{}')", list_url(ctx.team.id));
	card(&html! {
		(card_header("Add a server", Some("podkit connects over SSH, installs its agent and starts it. The server stays pending until that finishes.")))
		(card_content(&html! {
			form class="flex flex-col gap-4" "data-on:submit"=(post) {
				label class=(LABEL) {
					"Name"
					input class=(INPUT) type="text" required maxlength="60" placeholder="build-box" "data-bind:name";
				}
				div class="grid gap-4 sm:grid-cols-3" {
					div class="sm:col-span-2" {
						label class=(LABEL) {
							"Hostname or IP"
							input class=(INPUT) type="text" required placeholder="203.0.113.10" autocomplete="off" "data-bind:hostname";
						}
					}
					label class=(LABEL) {
						"SSH port"
						input class=(INPUT) type="number" required min="1" max="65535" "data-bind:ssh_port";
					}
				}
				label class=(LABEL) {
					"SSH user"
					input class=(INPUT) type="text" required placeholder="root" autocomplete="off" "data-bind:ssh_user";
				}
				label class=(LABEL) {
					"SSH private key"
					textarea class="min-h-32 w-full rounded-lg border border-border bg-transparent px-3 py-2 font-mono text-xs font-normal outline-hidden placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/50"
						required rows="6" spellcheck="false" autocomplete="off" autocapitalize="off"
						placeholder="Paste the private key podkit should log in with" "data-bind:ssh_private_key" {}
					span class="text-xs font-normal text-muted-foreground" {
						"Stored encrypted. It is never shown again, here or in the API."
					}
				}
				label class=(LABEL) {
					"Podman socket path"
					input class=(INPUT) type="text" required autocomplete="off" "data-bind:podman_socket_path";
					span class="text-xs font-normal text-muted-foreground" {
						"Where podman listens on that host. " code { "%t" }
						" is systemd's runtime directory: " code { "/run" }
						" for root, " code { "/run/user/<uid>" }
						" for a normal user. The default works for both."
					}
				}
				(form_error())
				div {
					button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Add server and bootstrap" }
				}
			}
		}))
	})
}

async fn list_page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let servers = service::list(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let can_create = ctx.perms.can(Action::Create, Resource::Server);
	let signals = format!(
		"{{name: '', hostname: '', ssh_port: {DEFAULT_SSH_PORT}, ssh_user: '', ssh_private_key: '', podman_socket_path: '{DEFAULT_PODMAN_SOCKET_PATH}', _error: ''}}"
	);

	let body = html! {
		div class="flex flex-col gap-6" data-signals=(signals) {
			(card(&html! {
				(card_header("Servers", Some("The podman hosts this team deploys to.")))
				(card_content(&server_list(&ctx, &servers)))
			}))
			@if can_create { (create_card(&ctx)) }
		}
	};
	Ok(app_page(&ctx, "Servers", &crumbs(&ctx), &body))
}

/// The polled re-render of the list.
async fn rows(State(s): State<AppState>, ctx: TeamCtx) -> Response {
	match service::list(&s.db, ctx.user.id, ctx.team.id).await {
		Ok(servers) => sse::patch(&server_list(&ctx, &servers)).into_response(),
		Err(e) => error::action(&e),
	}
}

/// A number or a numeric string: a number input's signal is one or the other
/// depending on whether the browser has parsed it yet.
#[derive(Deserialize)]
#[serde(untagged)]
enum Port {
	Number(i64),
	Text(String),
}

impl Port {
	fn value(&self) -> Option<i32> {
		match self {
			Self::Number(n) => i32::try_from(*n).ok(),
			Self::Text(t) => t.trim().parse().ok(),
		}
	}
}

#[derive(Deserialize)]
struct CreateSignals {
	name: String,
	hostname: String,
	ssh_port: Port,
	ssh_user: String,
	ssh_private_key: String,
	podman_socket_path: String,
}

async fn create(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<CreateSignals>,
) -> Response {
	let Some(ssh_port) = sig.ssh_port.value() else {
		return sse::error("SSH port must be between 1 and 65535.").into_response();
	};
	let new = NewServer {
		name: sig.name,
		hostname: sig.hostname.trim().to_owned(),
		ssh_port,
		ssh_user: sig.ssh_user.trim().to_owned(),
		ssh_private_key: sig.ssh_private_key,
		podman_socket_path: sig.podman_socket_path.trim().to_owned(),
	};
	match service::create(&s, ctx.user.id, ctx.team.id, new).await {
		Ok(server) => sse::redirect(&detail_url(ctx.team.id, server.id)).into_response(),
		Err(e) => error::action(&e),
	}
}

/// The message line of one action. Each action card has its own signal, so a
/// refusal shows only under the button that caused it.
fn action_error(signal: &str) -> Markup {
	html! {
		p class="text-sm text-destructive" role="alert" "data-show"=(format!("${signal}")) "data-text"=(format!("${signal}")) style="display: none" {}
	}
}

fn fact(label: &str, value: &Markup) -> Markup {
	html! {
		div class="flex flex-col gap-1 sm:flex-row sm:gap-4" {
			dt class="w-40 shrink-0 text-muted-foreground" { (label) }
			dd class="min-w-0 break-words" { (value) }
		}
	}
}

/// Status, message and facts of one server; patched in place by the poll.
fn status_region(ctx: &TeamCtx, server: &Server, job: Option<&Job>) -> Markup {
	let fingerprint = server.host_key.as_deref().and_then(host_key_fingerprint);
	let url = format!("{}/status", detail_url(ctx.team.id, server.id));
	let polling = server.status.in_flight() || job_in_flight(job);
	html! {
		div id="server-status" class="flex flex-col gap-4" {
			div class="flex flex-wrap items-center gap-2" {
				(status_badge(server.status))
				@if let Some(job) = job {
					span class="text-sm text-muted-foreground" { "Bootstrap " (job_label(job.status)) }
				}
			}
			@if let Some(message) = &server.status_message {
				p class="text-sm text-destructive" { (message) }
			}
			dl class="flex flex-col gap-3 text-sm" {
				(fact("Address", &html! { (host_port(server)) }))
				@if let Some(user) = &server.ssh_user {
					(fact("SSH user", &html! { (user) }))
				}
				(fact("Podman socket", &html! { span class="font-mono" { (server.podman_socket_path) } }))
				(fact("Agent version", &html! { (server.agent_version.as_deref().unwrap_or("not installed yet")) }))
				(fact("Architecture", &html! { (server.arch.as_deref().unwrap_or("unknown")) }))
				@if !server.is_local {
					(fact("Host key", &html! {
						@if let Some(fp) = &fingerprint {
							span class="font-mono text-xs break-all" { (fp) }
						} @else {
							"not pinned yet; the next bootstrap scans it"
						}
					}))
				}
			}
			(poll(&url, polling))
		}
	}
}

fn actions_card(ctx: &TeamCtx, server: &Server) -> Markup {
	let base = detail_url(ctx.team.id, server.id);
	let rerun = format!("@post('{base}/bootstrap')");
	let reset = format!("@post('{base}/bootstrap?reset_host_key=true')");
	card(&html! {
		(card_header("Bootstrap", Some("Reinstalls the agent and refreshes its service unit over SSH.")))
		(card_content(&html! {
			div class="flex flex-col gap-4" {
				div class="flex flex-wrap gap-3" {
					button type="button" class=(BUTTON) "data-on:click"=(rerun) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Re-run bootstrap" }
					button type="button" class=(BUTTON_OUTLINE) "data-on:click"="$_reset = !$_reset" { "Reset host key and bootstrap" }
				}
				div class="flex flex-col gap-3 rounded-lg border border-border p-4 text-sm" "data-show"="$_reset" style="display: none" {
					p {
						"podkit pins the server's SSH host key the first time it connects and refuses any other key afterwards. "
						"Reset it only when you know the host changed, for example after reinstalling its OS. "
						"If you did not change the host, a different key can mean someone is intercepting the connection."
					}
					div class="flex gap-3" {
						button type="button" class=(BUTTON_DANGER) "data-on:click"=(reset) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Forget the key and bootstrap" }
						button type="button" class=(BUTTON_OUTLINE) "data-on:click"="$_reset = false" { "Cancel" }
					}
				}
				(form_error())
			}
		}))
	})
}

fn delete_card(ctx: &TeamCtx, server: &Server) -> Markup {
	let post = format!("@post('{}/delete')", detail_url(ctx.team.id, server.id));
	card(&html! {
		(card_header("Delete server", Some("podkit forgets this server. Nothing is removed from the host itself.")))
		(card_content(&html! {
			div class="flex flex-col gap-4" {
				p class="text-sm" { "Type " strong { (server.name) } " to confirm." }
				input class=(INPUT) type="text" autocomplete="off" aria-label="Server name" "data-bind:confirm";
				(action_error("_delete_error"))
				div {
					button type="button" class=(BUTTON_DANGER) "data-on:click"=(post) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Delete server" }
				}
			}
		}))
	})
}

async fn detail(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
) -> Result<Markup, Response> {
	let found = service::get(&s.db, ctx.user.id, ctx.team.id, id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let server = &found.server;
	let can_update = ctx.perms.can(Action::Update, Resource::Server) && !server.is_local;
	let can_delete = ctx.perms.can(Action::Delete, Resource::Server) && !server.is_local;
	let log_url = format!("{}/log", detail_url(ctx.team.id, server.id));

	let mut crumbs = crumbs(&ctx);
	crumbs.push((server.name.as_str(), None));
	let body = html! {
		div class="flex flex-col gap-6" data-signals="{confirm: '', _reset: false, _error: '', _delete_error: ''}" {
			(card(&html! {
				div class="flex flex-wrap items-center gap-3 px-6" {
					h1 class="text-base leading-none font-semibold" { (server.name) }
					@if server.is_local { (local_badge()) }
				}
				(card_content(&status_region(&ctx, server, found.bootstrap.as_ref())))
			}))
			@if !server.is_local {
				(card(&html! {
					(card_header("Bootstrap log", None))
					(card_content(&html! {
						@if found.bootstrap.is_some() {
							(log_view(LOG_ID, &log_url))
						} @else {
							p class="text-sm text-muted-foreground" { "No bootstrap has run yet." }
						}
					}))
				}))
			}
			@if can_update { (actions_card(&ctx, server)) }
			@if can_delete { (delete_card(&ctx, server)) }
		}
	};
	Ok(app_page(&ctx, &server.name, &crumbs, &body))
}

/// The polled re-render of the status region.
async fn status(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
) -> Response {
	match service::get(&s.db, ctx.user.id, ctx.team.id, id).await {
		Ok(found) => sse::patch(&status_region(
			&ctx,
			&found.server,
			found.bootstrap.as_ref(),
		))
		.into_response(),
		Err(e) => error::action(&e),
	}
}

/// The newest bootstrap job's log as Datastar patches.
async fn bootstrap_log(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
) -> Response {
	match service::get(&s.db, ctx.user.id, ctx.team.id, id).await {
		Ok(found) => job_log(&s, found.bootstrap.map(|j| j.id), LOG_ID),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize)]
struct BootstrapQuery {
	#[serde(default)]
	reset_host_key: bool,
}

async fn bootstrap(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
	Query(query): Query<BootstrapQuery>,
) -> Response {
	match service::bootstrap(&s, ctx.user.id, ctx.team.id, id, query.reset_host_key).await {
		// A reload shows the new job's log from its first line.
		Ok(server) => sse::redirect(&detail_url(ctx.team.id, server.id)).into_response(),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize)]
struct DeleteSignals {
	confirm: String,
}

async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
	ReadSignals(sig): ReadSignals<DeleteSignals>,
) -> Response {
	let result = async {
		// The name is checked after a read so a mistyped name never reaches
		// the delete; `remove` still authorizes the delete itself.
		let found = service::get(&s.db, ctx.user.id, ctx.team.id, id).await?;
		if sig.confirm != found.server.name {
			return Ok(None);
		}
		service::remove(&s, ctx.user.id, ctx.team.id, id).await?;
		Ok::<_, ServerError>(Some(found.server))
	}
	.await;
	match result {
		Ok(Some(_)) => sse::redirect(&list_url(ctx.team.id))
			.and(sse::signals(&json!({ "confirm": "", "_delete_error": "" })))
			.into_response(),
		Ok(None) => delete_error("The name doesn't match. Type it exactly to confirm."),
		Err(e) => delete_error(&error::message(&e)),
	}
}

fn delete_error(message: &str) -> Response {
	sse::signals(&json!({ "_delete_error": message })).into_response()
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use axum::Router;
	use axum::http::StatusCode;
	use serde_json::{Value, json};

	use crate::ids::ServerId;
	use crate::jobs::{JobKind, latest_for};
	use crate::state::AppState;
	use crate::tests::{
		PgPool, call, count, db_test, project_id, seed_application_on, state, user_with_team,
	};
	use crate::ui::testing::{
		assert_contains, assert_not_contains, get_html, post_signals, ui_login, ui_session,
	};

	const KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----\n";
	const ED25519: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJchJVs7z1tmicd4SpMN5UdQGOeEF7c+3zkUwGLv44BI";
	const ED25519_FP: &str = "SHA256:ywBw9jYLna1cd5Li260999IYfpxB/BrS/G6GhT1Xkwg";

	async fn app_and_state(db: PgPool) -> (Router, AppState) {
		let state = state(db);
		let app = crate::routes::routes(state.clone()).await.unwrap();
		(app, state)
	}

	fn new_server(name: &str) -> Value {
		json!({
			"name": name, "hostname": "203.0.113.10", "ssh_port": 22, "ssh_user": "root",
			"ssh_private_key": KEY, "podman_socket_path": "%t/podman/podman.sock",
		})
	}

	/// Adds a server through the UI and returns its id.
	async fn add(app: &Router, cookie: &str, team: i64, name: &str) -> i64 {
		let (s, _, body) = post_signals(
			app,
			&format!("/teams/{team}/servers"),
			Some(cookie),
			&new_server(name),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, &format!("/teams/{team}/servers/"));
		add_id_from(&body, team)
	}

	/// The id and name of the team's local server.
	async fn local_server(db: &PgPool, team: i64) -> (i64, String) {
		let server = crate::servers::list_by_team(db, crate::ids::TeamId(team))
			.await
			.unwrap()
			.into_iter()
			.find(|s| s.is_local)
			.unwrap();
		(server.id.0, server.name)
	}

	#[db_test]
	async fn the_list_shows_the_local_server_and_the_form(db: PgPool) {
		let (app, _) = app_and_state(db.clone()).await;
		let (cookie, team) = ui_session(&app, "alice").await;

		let (s, _, body) = get_html(&app, &format!("/teams/{team}/servers"), Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "<title>Servers");
		assert_contains(&body, "Local");
		assert_contains(&body, "Active");
		assert_contains(&body, "Add a server");
		assert_contains(&body, "SSH private key");
		assert_contains(&body, "%t/podman/podman.sock");
		assert_contains(&body, "systemd");
		assert_contains(&body, "ssh_port: 22");
		assert_not_contains(&body, "BEGIN");
	}

	#[db_test]
	async fn adding_a_server_bootstraps_it_and_never_echoes_the_key(db: PgPool) {
		let (app, _) = app_and_state(db.clone()).await;
		let (cookie, team) = ui_session(&app, "alice").await;

		let (s, _, created) = post_signals(
			&app,
			&format!("/teams/{team}/servers"),
			Some(&cookie),
			&new_server("edge-1"),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		assert_not_contains(&created, "BEGIN");
		let id = add_id_from(&created, team);
		assert_eq!(count(&db, "jobs").await, 1);

		// The port may arrive as text; a second server proves it.
		let mut other = new_server("edge-2");
		other["ssh_port"] = json!("2222");
		let (_, _, body) = post_signals(
			&app,
			&format!("/teams/{team}/servers"),
			Some(&cookie),
			&other,
		)
		.await;
		assert_contains(&body, &format!("/teams/{team}/servers/"));
		assert_eq!(count(&db, "jobs").await, 2);

		let (s, _, list) = get_html(&app, &format!("/teams/{team}/servers"), Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&list, "edge-1");
		assert_contains(&list, "203.0.113.10:22");
		assert_contains(&list, "203.0.113.10:2222");
		assert_contains(&list, "Pending");
		assert_contains(&list, "/servers/rows");
		assert_not_contains(&list, "BEGIN");

		let (s, _, rows) =
			get_html(&app, &format!("/teams/{team}/servers/rows"), Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&rows, "server-list");
		assert_not_contains(&rows, "BEGIN");

		let (s, _, page) =
			get_html(&app, &format!("/teams/{team}/servers/{id}"), Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&page, "edge-1");
		assert_contains(&page, "Bootstrap queued");
		assert_not_contains(&page, "BEGIN");
		let (_, _, status) = get_html(
			&app,
			&format!("/teams/{team}/servers/{id}/status"),
			Some(&cookie),
		)
		.await;
		assert_not_contains(&status, "BEGIN");

		// Stored encrypted, and the JSON API agrees there is no key to read.
		let token = ui_token(&app).await;
		let (_, json) = call(
			&app,
			"GET",
			&format!("/api/v1/teams/{team}/servers"),
			Some(&token),
			None,
		)
		.await;
		assert_not_contains(&json.to_string(), "BEGIN");
		assert_not_contains(&json.to_string(), "ssh_private_key");
	}

	fn add_id_from(body: &str, team: i64) -> i64 {
		body.split(&format!("/teams/{team}/servers/"))
			.nth(1)
			.unwrap()
			.split(|c: char| !c.is_ascii_digit())
			.next()
			.unwrap()
			.parse()
			.unwrap()
	}

	async fn ui_token(app: &Router) -> String {
		let creds = json!({ "email": "alice@example.com", "password": "correct horse battery" });
		let (_, login) = call(app, "POST", "/api/v1/auth/login", None, Some(creds)).await;
		login["token"].as_str().unwrap().to_owned()
	}

	#[db_test]
	async fn bad_input_shows_a_message_and_creates_nothing(db: PgPool) {
		let (app, _) = app_and_state(db.clone()).await;
		let (cookie, team) = ui_session(&app, "alice").await;
		let uri = format!("/teams/{team}/servers");

		let cases = [
			("name", json!(""), "Name must be"),
			("hostname", json!("not a host"), "invalid hostname"),
			("ssh_user", json!("Root User"), "invalid ssh_user"),
			("ssh_port", json!(70000), "SSH port"),
			("ssh_port", json!("abc"), "SSH port"),
			(
				"podman_socket_path",
				json!("relative/path"),
				"podman_socket_path",
			),
			("ssh_private_key", json!("  "), "private key is required"),
		];
		for (field, value, message) in cases {
			let mut body = new_server("edge");
			body[field] = value;
			let (s, _, out) = post_signals(&app, &uri, Some(&cookie), &body).await;
			assert_eq!(s, StatusCode::OK);
			assert_contains(&out, message);
			assert_not_contains(&out, "BEGIN");
		}
		assert_eq!(count(&db, "servers").await, 1, "only the local server");
		assert_eq!(count(&db, "jobs").await, 0);
	}

	#[db_test]
	async fn the_detail_page_streams_the_bootstrap_log(db: PgPool) {
		let (app, state) = app_and_state(db.clone()).await;
		let (cookie, team) = ui_session(&app, "alice").await;
		let id = add(&app, &cookie, team, "edge-1").await;
		let job = latest_for(&db, JobKind::ServerBootstrap, id)
			.await
			.unwrap()
			.unwrap();

		let page = format!("/teams/{team}/servers/{id}");
		let (_, _, body) = get_html(&app, &page, Some(&cookie)).await;
		assert_contains(&body, "Bootstrap log");
		assert_contains(&body, &format!("@get('{page}/log')"));
		assert_contains(&body, "id=\"bootstrap-log\"");

		let mut log = state.logs.open(job.id).await.unwrap();
		log.push(proto::Stream::System, "connecting to 203.0.113.10")
			.await;
		log.push(proto::Stream::Stdout, "<script>alert(1)</script>")
			.await;
		let reader = {
			let (app, cookie, uri) = (app.clone(), cookie.clone(), format!("{page}/log"));
			tokio::spawn(async move { get_html(&app, &uri, Some(&cookie)).await })
		};
		tokio::time::sleep(Duration::from_millis(200)).await;
		log.push(proto::Stream::Stdout, "agent installed").await;
		log.finish().await;

		let (s, _, stream) = tokio::time::timeout(Duration::from_secs(5), reader)
			.await
			.expect("log stream ends with the job")
			.unwrap();
		assert_eq!(s, StatusCode::OK);
		assert_contains(&stream, "connecting to 203.0.113.10");
		assert_contains(&stream, "agent installed");
		assert_contains(&stream, "&lt;script&gt;alert(1)&lt;/script&gt;");
		assert_not_contains(&stream, "<script>");
		assert_contains(&stream, "#bootstrap-log");
		assert_contains(&stream, "\"_live\":false");
	}

	#[db_test]
	async fn the_log_of_a_server_with_no_job_ends_at_once(db: PgPool) {
		let (app, _) = app_and_state(db.clone()).await;
		let (cookie, team) = ui_session(&app, "alice").await;
		let (local, _) = local_server(&db, team).await;

		let (s, _, body) = get_html(
			&app,
			&format!("/teams/{team}/servers/{local}/log"),
			Some(&cookie),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "\"_live\":false");
	}

	#[db_test]
	async fn reset_host_key_sends_the_flag_and_forgets_the_key_a_plain_rerun_keeps_it(db: PgPool) {
		let (app, _) = app_and_state(db.clone()).await;
		let (cookie, team) = ui_session(&app, "alice").await;
		let id = add(&app, &cookie, team, "edge-1").await;
		let page = format!("/teams/{team}/servers/{id}");
		let line = format!("203.0.113.10 ssh-ed25519 {ED25519}");
		crate::servers::set_host_key(&db, ServerId(id), Some(&line))
			.await
			.unwrap();

		let (_, _, body) = get_html(&app, &page, Some(&cookie)).await;
		assert_contains(&body, ED25519_FP);
		assert_contains(
			&body,
			&format!("@post('{page}/bootstrap?reset_host_key=true')"),
		);
		assert_contains(&body, &format!("@post('{page}/bootstrap')"));
		assert_contains(&body, "Reset it only when you know the host changed");

		let (_, _, out) = post_signals(
			&app,
			&format!("{page}/bootstrap"),
			Some(&cookie),
			&json!({}),
		)
		.await;
		assert_contains(&out, &page);
		let (_, _, body) = get_html(&app, &page, Some(&cookie)).await;
		assert_contains(&body, ED25519_FP);
		assert_eq!(count(&db, "jobs").await, 2);

		let (_, _, out) = post_signals(
			&app,
			&format!("{page}/bootstrap?reset_host_key=true"),
			Some(&cookie),
			&json!({}),
		)
		.await;
		assert_contains(&out, &page);
		let (_, _, body) = get_html(&app, &page, Some(&cookie)).await;
		assert_not_contains(&body, ED25519_FP);
		assert_contains(&body, "not pinned yet");
		assert_eq!(count(&db, "jobs").await, 3);
	}

	#[db_test]
	async fn the_local_server_has_no_bootstrap_or_delete_and_refuses_both(db: PgPool) {
		let (app, _) = app_and_state(db.clone()).await;
		let (cookie, team) = ui_session(&app, "alice").await;
		let (local, local_name) = local_server(&db, team).await;
		let page = format!("/teams/{team}/servers/{local}");

		let (s, _, body) = get_html(&app, &page, Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "Local");
		assert_not_contains(&body, "Re-run bootstrap");
		assert_not_contains(&body, "Delete server");

		let (_, _, out) = post_signals(
			&app,
			&format!("{page}/bootstrap"),
			Some(&cookie),
			&json!({}),
		)
		.await;
		assert_contains(&out, "managed by the operator");
		let (_, _, out) = post_signals(
			&app,
			&format!("{page}/delete"),
			Some(&cookie),
			&json!({ "confirm": local_name }),
		)
		.await;
		assert_contains(&out, "cannot be deleted");
		assert_eq!(count(&db, "servers").await, 1);
	}

	#[db_test]
	async fn deleting_needs_the_exact_name_and_shows_refusals_inline(db: PgPool) {
		let (app, _) = app_and_state(db.clone()).await;
		let (cookie, team) = ui_session(&app, "alice").await;
		let id = add(&app, &cookie, team, "edge-1").await;
		let uri = format!("/teams/{team}/servers/{id}/delete");

		let (_, _, body) =
			post_signals(&app, &uri, Some(&cookie), &json!({ "confirm": "edge" })).await;
		assert_contains(&body, "doesn't match");
		assert_eq!(count(&db, "servers").await, 2);

		let (_, _, body) =
			post_signals(&app, &uri, Some(&cookie), &json!({ "confirm": "edge-1" })).await;
		assert_contains(&body, &format!("/teams/{team}/servers"));
		assert_eq!(count(&db, "servers").await, 1);

		// A server that still hosts an application is refused with a message.
		let id = add(&app, &cookie, team, "edge-2").await;
		post_signals(
			&app,
			&format!("/teams/{team}/projects"),
			Some(&cookie),
			&json!({ "name": "Shop", "slug": "shop" }),
		)
		.await;
		let project = project_id(&db, "shop").await;
		seed_application_on(&db, project, id).await;
		let (_, _, body) = post_signals(
			&app,
			&format!("/teams/{team}/servers/{id}/delete"),
			Some(&cookie),
			&json!({ "confirm": "edge-2" }),
		)
		.await;
		assert_contains(&body, "still runs applications or databases");
		assert_eq!(count(&db, "servers").await, 2);
	}

	#[db_test]
	async fn a_name_with_markup_is_escaped_everywhere(db: PgPool) {
		let (app, _) = app_and_state(db.clone()).await;
		let (cookie, team) = ui_session(&app, "alice").await;
		let name = "<script>alert(1)</script>";
		let id = add(&app, &cookie, team, name).await;

		for uri in [
			format!("/teams/{team}/servers"),
			format!("/teams/{team}/servers/rows"),
			format!("/teams/{team}/servers/{id}"),
		] {
			let (s, _, body) = get_html(&app, &uri, Some(&cookie)).await;
			assert_eq!(s, StatusCode::OK, "{uri}");
			assert_not_contains(&body, name);
			assert_contains(&body, "&lt;script&gt;alert(1)&lt;/script&gt;");
		}
	}

	#[db_test]
	async fn another_teams_server_is_the_404_page_and_cannot_be_touched(db: PgPool) {
		let (app, _) = app_and_state(db.clone()).await;
		let (alice, alice_team) = ui_session(&app, "alice").await;
		let (bob, bob_team) = ui_session(&app, "bob").await;
		let secret = add(&app, &alice, alice_team, "secret-box").await;

		for uri in [
			format!("/teams/{alice_team}/servers"),
			format!("/teams/{alice_team}/servers/{secret}"),
			format!("/teams/{bob_team}/servers/{secret}"),
			"/teams/999/servers".to_owned(),
		] {
			let (s, _, body) = get_html(&app, &uri, Some(&bob)).await;
			assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
			assert_contains(&body, "Not found");
			assert_not_contains(&body, "secret-box");
			assert_not_contains(&body, "203.0.113.10");
		}

		// The streams and polls answer a Datastar GET: a message, never data.
		for uri in [
			format!("/teams/{bob_team}/servers/{secret}/log"),
			format!("/teams/{bob_team}/servers/{secret}/status"),
		] {
			let (_, _, out) = get_html(&app, &uri, Some(&bob)).await;
			assert_contains(&out, "That no longer exists.");
			assert_not_contains(&out, "secret-box");
			assert_not_contains(&out, "_live");
		}
		for uri in [
			format!("/teams/{alice_team}/servers/{secret}/status"),
			format!("/teams/{alice_team}/servers/{secret}/log"),
			format!("/teams/{alice_team}/servers/rows"),
		] {
			let (s, _, out) = get_html(&app, &uri, Some(&bob)).await;
			assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
			assert_not_contains(&out, "secret-box");
		}

		for uri in [
			format!("/teams/{alice_team}/servers"),
			format!("/teams/{bob_team}/servers/{secret}/bootstrap"),
			format!("/teams/{bob_team}/servers/{secret}/bootstrap?reset_host_key=true"),
			format!("/teams/{bob_team}/servers/{secret}/delete"),
		] {
			let mut body = new_server("planted");
			body["confirm"] = json!("secret-box");
			let (_, _, out) = post_signals(&app, &uri, Some(&bob), &body).await;
			assert_contains(&out, "That no longer exists.");
		}
		assert_eq!(count(&db, "servers").await, 3);
		assert_eq!(count(&db, "jobs").await, 1);
	}

	#[db_test]
	async fn a_read_only_member_sees_no_form_or_actions_and_forged_posts_fail(db: PgPool) {
		let (app, _) = app_and_state(db.clone()).await;
		let (owner_token, team) = user_with_team(&app, "alice").await;
		let owner = ui_login(&app, "alice").await;
		let (_, _) = user_with_team(&app, "carol").await;
		let (s, _) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/members"),
			Some(&owner_token),
			Some(json!({ "email": "carol@example.com", "role_id": 2 })),
		)
		.await;
		assert!(s.is_success());
		let carol = ui_login(&app, "carol").await;
		let id = add(&app, &owner, team, "edge-1").await;
		let jobs_before = count(&db, "jobs").await;

		let (s, _, list) = get_html(&app, &format!("/teams/{team}/servers"), Some(&carol)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&list, "edge-1");
		assert_not_contains(&list, "Add a server");
		assert_not_contains(&list, "SSH private key");

		let (s, _, page) =
			get_html(&app, &format!("/teams/{team}/servers/{id}"), Some(&carol)).await;
		assert_eq!(s, StatusCode::OK);
		assert_not_contains(&page, "Re-run bootstrap");
		assert_not_contains(&page, "Delete server");

		let forbidden = "You don't have permission to do that.";
		let (_, _, out) = post_signals(
			&app,
			&format!("/teams/{team}/servers"),
			Some(&carol),
			&new_server("planted"),
		)
		.await;
		assert_contains(&out, forbidden);
		assert_not_contains(&out, "BEGIN");
		for uri in [
			format!("/teams/{team}/servers/{id}/bootstrap"),
			format!("/teams/{team}/servers/{id}/bootstrap?reset_host_key=true"),
			format!("/teams/{team}/servers/{id}/delete"),
		] {
			let (_, _, out) =
				post_signals(&app, &uri, Some(&carol), &json!({ "confirm": "edge-1" })).await;
			assert_contains(&out, forbidden);
		}
		assert_eq!(
			count(&db, "servers").await,
			3,
			"two local servers and edge-1"
		);
		assert_eq!(count(&db, "jobs").await, jobs_before);
	}
}
