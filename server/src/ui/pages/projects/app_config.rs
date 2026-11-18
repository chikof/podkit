//! The Environment, Domains and Volumes tabs of an application.
//!
//! Per-app config that takes effect on the next deploy, so every change shows
//! the same "Saved" banner. Env var values are write-only: the forms send
//! them, and nothing here (HTML, signal patch or SSE event) ever carries one
//! back. Lists show keys only.

use std::net::IpAddr;

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::apps::{self, Tab};
use crate::applications::service::ApplicationView;
use crate::auth::permission::{Action, Resource};
use crate::domains::Domain;
use crate::domains::service as domains;
use crate::env_vars::service::{self as env_vars, BulkReport, EnvVarKey};
use crate::error::ServerError;
use crate::ids::{ApplicationId, DomainId, EnvVarId, ProjectId, TeamId, VolumeId};
use crate::projects::Project;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, BUTTON_DANGER, BUTTON_OUTLINE, INPUT, LABEL, TEXTAREA, card, card_content, card_header,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::form_error;
use crate::ui::{error, sse};
use crate::volumes::Volume;
use crate::volumes::service as volumes;

const SAVED: &str = "Saved. Changes apply on the next deploy.";

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/env",
			get(env_page).post(env_set),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/env/bulk",
			post(env_bulk),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/env/{env_var_id}/delete",
			post(env_remove),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/domains",
			get(domains_page).post(domain_add),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/domains/{domain_id}/delete",
			post(domain_remove),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/volumes",
			get(volumes_page).post(volume_add),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/volumes/{volume_id}/delete",
			post(volume_remove),
		)
}

// --- shared pieces -----------------------------------------------------------

/// The slot the "Saved" banner is patched into. Empty until a change lands.
fn saved_slot(saved: bool) -> Markup {
	html! {
		div id="saved" {
			@if saved {
				div class="rounded-lg border border-success/30 bg-success/15 px-4 py-3 text-sm" role="status" { (SAVED) }
			}
		}
	}
}

fn empty_row(text: &str) -> Markup {
	html! { p class="border-t border-border px-6 py-4 text-sm text-muted-foreground" { (text) } }
}

const ROW: &str =
	"flex flex-wrap items-center justify-between gap-3 border-t border-border px-6 py-3";

fn base(ctx: &TeamCtx, project: &Project, app: &ApplicationView) -> String {
	apps::app_url(ctx.team.id, project.id, app.id)
}

fn fail(ctx: &TeamCtx) -> impl Fn(ServerError) -> Response + '_ {
	move |e| error::page(ctx, &e)
}

// --- environment variables -----------------------------------------------------

fn env_list(base: &str, keys: &[EnvVarKey], can_delete: bool) -> Markup {
	html! {
		div id="env-list" {
			@if keys.is_empty() {
				(empty_row("No environment variables yet."))
			}
			@for var in keys {
				div class=(ROW) {
					div class="flex min-w-0 flex-col" {
						code class="font-mono text-sm font-medium break-all" { (var.key) }
						span class="text-xs text-muted-foreground" { "Value hidden" }
					}
					@if can_delete {
						button type="button" class=(BUTTON_DANGER) "data-on:click"={ "@post('" (base) "/env/" (var.id) "/delete')" } { "Delete" }
					}
				}
			}
		}
	}
}

fn bulk_report(report: Option<&BulkReport>) -> Markup {
	html! {
		div id="bulk-result" {
			@if let Some(report) = report {
				div class="flex flex-col gap-1 text-sm" role="status" {
					p {
						"Saved " (report.saved) @if report.saved == 1 { " variable." } @else { " variables." }
					}
					@if !report.errors.is_empty() {
						p class="text-destructive" { "These lines were skipped. Fix them and paste again:" }
						ul class="list-disc pl-5 text-destructive" {
							@for e in &report.errors {
								li { "Line " (e.line) ": " (e.message) "." }
							}
						}
					}
				}
			}
		}
	}
}

fn bulk_error() -> Markup {
	html! {
		p class="text-sm text-destructive" role="alert" "data-show"="$_bulk_error" "data-text"="$_bulk_error" style="display: none" {}
	}
}

async fn env_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	let (project, app) = apps::load(&s, &ctx, project, app).await?;
	let keys = env_vars::list(&s.db, ctx.user.id, ctx.team.id, project.id, app.id)
		.await
		.map_err(fail(&ctx))?;
	let base = base(&ctx, &project, &app);
	let can_create = ctx.perms.can(Action::Create, Resource::EnvVar);
	let can_delete = ctx.perms.can(Action::Delete, Resource::EnvVar);

	let set = format!("evt.preventDefault(); @post('{base}/env')");
	let bulk = format!("@post('{base}/env/bulk')");
	let content = html! {
		div class="flex flex-col gap-6" data-signals=(json!({
			"env_key": "", "env_value": "", "env_bulk": "", "_error": "", "_bulk_error": "",
		}).to_string()) {
			(saved_slot(false))
			(card(&html! {
				(card_header("Environment variables", Some("Values are encrypted and never shown again. Setting a name that exists replaces its value.")))
				div class="flex flex-col gap-2" {
					(form_error())
					(env_list(&base, &keys, can_delete))
				}
			}))
			@if can_create {
				(card(&html! {
					(card_header("Add or update a variable", None))
					(card_content(&html! {
						form class="flex flex-col gap-4" "data-on:submit"=(set) {
							label class=(LABEL) {
								"Name"
								input class=(INPUT) type="text" required maxlength="255" placeholder="DATABASE_URL" autocomplete="off" spellcheck="false" "data-bind:env_key";
							}
							label class=(LABEL) {
								"Value"
								textarea class=(TEXTAREA) rows="3" autocomplete="off" spellcheck="false" "data-bind:env_value" {}
							}
							div {
								button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Save variable" }
							}
						}
					}))
				}))
				(card(&html! {
					(card_header("Paste a .env file", Some("One KEY=VALUE per line. Blank lines and lines starting with # are ignored. Lines that don't parse are skipped and reported by number.")))
					(card_content(&html! {
						div class="flex flex-col gap-4" {
							textarea class=(TEXTAREA) rows="8" autocomplete="off" spellcheck="false" aria-label="Contents of a .env file" placeholder="KEY=value" "data-bind:env_bulk" {}
							(bulk_error())
							(bulk_report(None))
							div {
								button type="button" class=(BUTTON) "data-on:click"=(bulk) "data-indicator:_bulk_busy" "data-attr:disabled"="$_bulk_busy" { "Import variables" }
							}
						}
					}))
				}))
			}
		}
	};
	Ok(apps::shell(&s, &ctx, &project, &app, Tab::Env, &content))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct EnvSignals {
	env_key: String,
	env_value: String,
}

async fn env_set(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	ReadSignals(sig): ReadSignals<EnvSignals>,
) -> Response {
	let result = async {
		env_vars::upsert(
			&s,
			ctx.user.id,
			ctx.team.id,
			project,
			app,
			sig.env_key.trim(),
			&sig.env_value,
		)
		.await?;
		env_vars::list(&s.db, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(keys) => {
			let base = apps::app_url(ctx.team.id, project, app);
			let can_delete = ctx.perms.can(Action::Delete, Resource::EnvVar);
			sse::patch(&env_list(&base, &keys, can_delete))
				.and(sse::patch(&saved_slot(true)))
				.and(sse::signals(
					&json!({ "env_key": "", "env_value": "", "_error": "" }),
				))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct BulkSignals {
	env_bulk: String,
}

async fn env_bulk(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	ReadSignals(sig): ReadSignals<BulkSignals>,
) -> Response {
	let result = async {
		let report =
			env_vars::bulk_set(&s, ctx.user.id, ctx.team.id, project, app, &sig.env_bulk).await?;
		let keys = env_vars::list(&s.db, ctx.user.id, ctx.team.id, project, app).await?;
		Ok::<_, ServerError>((report, keys))
	}
	.await;
	match result {
		Ok((report, keys)) => {
			let base = apps::app_url(ctx.team.id, project, app);
			let can_delete = ctx.perms.can(Action::Delete, Resource::EnvVar);
			let mut events = sse::patch(&bulk_report(Some(&report)))
				.and(sse::patch(&env_list(&base, &keys, can_delete)))
				.and(sse::signals(&json!({ "_bulk_error": "" })));
			if report.saved > 0 {
				events = events.and(sse::patch(&saved_slot(true)));
			}
			// Keep the text when lines were skipped, so they can be fixed.
			if report.errors.is_empty() {
				events = events.and(sse::signals(&json!({ "env_bulk": "" })));
			}
			events.into_response()
		}
		Err(e) => sse::signals(&json!({ "_bulk_error": error::message(&e) })).into_response(),
	}
}

async fn env_remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, EnvVarId)>,
) -> Response {
	let result = async {
		env_vars::remove(&s.db, ctx.user.id, ctx.team.id, project, app, id).await?;
		env_vars::list(&s.db, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(keys) => {
			let base = apps::app_url(ctx.team.id, project, app);
			let can_delete = ctx.perms.can(Action::Delete, Resource::EnvVar);
			sse::patch(&env_list(&base, &keys, can_delete))
				.and(sse::patch(&saved_slot(true)))
				.and(sse::signals(&json!({ "_error": "" })))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

// --- domains -------------------------------------------------------------------

fn domain_list(base: &str, domains: &[Domain], can_delete: bool) -> Markup {
	html! {
		div id="domain-list" {
			@if domains.is_empty() {
				(empty_row("No custom domains yet."))
			}
			@for domain in domains {
				div class=(ROW) {
					span class="text-sm font-medium break-all" { (domain.hostname) }
					@if can_delete {
						button type="button" class=(BUTTON_DANGER) "data-on:click"={ "@post('" (base) "/domains/" (domain.id) "/delete')" } { "Remove" }
					}
				}
			}
		}
	}
}

/// What to put in DNS: an address record when the server is reached by IP,
/// else an alias to its name.
fn dns_help(server_host: &str) -> Markup {
	let is_ip = server_host.parse::<IpAddr>().is_ok();
	html! {
		div class="flex flex-col gap-3 text-sm" {
			@if is_ip {
				p { "At your DNS provider, add an " strong { "A" } " record (" strong { "AAAA" } " for an IPv6 address) for the hostname pointing to:" }
			} @else {
				p { "At your DNS provider, add a " strong { "CNAME" } " record for the hostname pointing to:" }
			}
			code class="w-fit rounded-md border border-border bg-foreground/5 px-2 py-1 font-mono break-all" { (server_host) }
			@if !is_ip {
				p class="text-muted-foreground" { "A bare domain such as example.com can't have a CNAME; use an A record with the server's IP address instead." }
			}
			p class="text-muted-foreground" { "podkit doesn't check DNS. The domain starts to answer after the next deploy, once the record has propagated." }
		}
	}
}

fn https_banner() -> Markup {
	html! {
		div class="rounded-lg border border-warning/30 bg-warning/15 px-4 py-3 text-sm" role="note" {
			strong { "HTTPS is off for custom domains. " }
			"No ACME email is configured on this podkit server, so custom domains are served over plain HTTP. Set "
			code class="font-mono" { "ACME_EMAIL" }
			" in the server's configuration and restart it to get automatic Let's Encrypt certificates."
		}
	}
}

async fn domains_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	let (project, app) = apps::load(&s, &ctx, project, app).await?;
	let list = domains::list(&s.db, ctx.user.id, ctx.team.id, project.id, app.id)
		.await
		.map_err(fail(&ctx))?;
	let base = base(&ctx, &project, &app);
	let can_create = ctx.perms.can(Action::Create, Resource::Domain);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Domain);

	let add = format!("evt.preventDefault(); @post('{base}/domains')");
	let content = html! {
		div class="flex flex-col gap-6" data-signals=(json!({ "domain_hostname": "", "_error": "" }).to_string()) {
			@if s.config.acme_email.is_none() {
				(https_banner())
			}
			(saved_slot(false))
			(card(&html! {
				(card_header("Custom domains", Some("Hostnames that route to this application, besides its generated one.")))
				div class="flex flex-col gap-2" {
					(form_error())
					(domain_list(&base, &list.domains, can_delete))
				}
			}))
			@if can_create {
				(card(&html! {
					(card_header("Add a domain", None))
					(card_content(&html! {
						form class="flex flex-col gap-4" "data-on:submit"=(add) {
							label class=(LABEL) {
								"Hostname"
								input class=(INPUT) type="text" required placeholder="app.example.com" autocomplete="off" spellcheck="false" autocapitalize="off" "data-bind:domain_hostname";
							}
							div {
								button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Add domain" }
							}
						}
					}))
				}))
			}
			(card(&html! {
				(card_header("DNS setup", None))
				(card_content(&dns_help(&list.server_host)))
			}))
		}
	};
	Ok(apps::shell(
		&s,
		&ctx,
		&project,
		&app,
		Tab::Domains,
		&content,
	))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DomainSignals {
	domain_hostname: String,
}

async fn domain_add(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	ReadSignals(sig): ReadSignals<DomainSignals>,
) -> Response {
	let result = async {
		domains::create(
			&s.db,
			ctx.user.id,
			ctx.team.id,
			project,
			app,
			sig.domain_hostname.trim(),
		)
		.await?;
		domains::list(&s.db, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(list) => {
			let base = apps::app_url(ctx.team.id, project, app);
			let can_delete = ctx.perms.can(Action::Delete, Resource::Domain);
			sse::patch(&domain_list(&base, &list.domains, can_delete))
				.and(sse::patch(&saved_slot(true)))
				.and(sse::signals(
					&json!({ "domain_hostname": "", "_error": "" }),
				))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn domain_remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DomainId)>,
) -> Response {
	let result = async {
		domains::remove(&s.db, ctx.user.id, ctx.team.id, project, app, id).await?;
		domains::list(&s.db, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(list) => {
			let base = apps::app_url(ctx.team.id, project, app);
			let can_delete = ctx.perms.can(Action::Delete, Resource::Domain);
			sse::patch(&domain_list(&base, &list.domains, can_delete))
				.and(sse::patch(&saved_slot(true)))
				.and(sse::signals(&json!({ "_error": "" })))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

// --- volumes -------------------------------------------------------------------

/// Each row asks for confirmation before it deletes: `_confirm` holds the id
/// of the volume being confirmed (empty for none) and is never sent to the
/// server. It is a string: ids are 64-bit and a JS number would round them.
fn volume_list(base: &str, volumes: &[Volume], can_delete: bool) -> Markup {
	html! {
		div id="volume-list" {
			@if volumes.is_empty() {
				(empty_row("No volumes yet. Without one, files written by the app are lost on every deploy."))
			}
			@for volume in volumes {
				div class="flex flex-col gap-3 border-t border-border px-6 py-3" {
					div class="flex flex-wrap items-center justify-between gap-3" {
						div class="flex min-w-0 flex-col" {
							code class="font-mono text-sm font-medium break-all" { (volume.mount_path) }
							span class="text-xs text-muted-foreground break-all" { (volume.name) }
						}
						@if can_delete {
							button type="button" class=(BUTTON_DANGER) "data-on:click"={ "$_confirm = '" (volume.id) "'" } "data-show"={ "$_confirm !== '" (volume.id) "'" } { "Delete" }
						}
					}
					@if can_delete {
						div class="flex flex-col gap-3 rounded-lg border border-destructive/30 bg-destructive/15 p-4 text-sm" role="alert" "data-show"={ "$_confirm === '" (volume.id) "'" } style="display: none" {
							p {
								strong { "This permanently deletes all data in " (volume.mount_path) "." }
								" The files are removed from the server and can't be recovered. The application must be stopped first."
							}
							div class="flex flex-wrap gap-2" {
								button type="button" class=(BUTTON_DANGER) "data-on:click"={ "@post('" (base) "/volumes/" (volume.id) "/delete')" } "data-indicator:_deleting" "data-attr:disabled"="$_deleting" { "Delete volume and its data" }
								button type="button" class=(BUTTON_OUTLINE) "data-on:click"="$_confirm = ''" { "Cancel" }
							}
						}
					}
				}
			}
		}
	}
}

async fn volumes_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	let (project, app) = apps::load(&s, &ctx, project, app).await?;
	let list = volumes::list(&s.db, ctx.user.id, ctx.team.id, project.id, app.id)
		.await
		.map_err(fail(&ctx))?;
	let base = base(&ctx, &project, &app);
	let can_create = ctx.perms.can(Action::Create, Resource::Volume);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Volume);

	let add = format!("evt.preventDefault(); @post('{base}/volumes')");
	let content = html! {
		div class="flex flex-col gap-6" data-signals=(json!({ "volume_path": "", "_confirm": "", "_error": "" }).to_string()) {
			(saved_slot(false))
			(card(&html! {
				(card_header("Volumes", Some("Persistent storage mounted into the container. A volume keeps its files across deploys and is mounted from the next deploy on.")))
				div class="flex flex-col gap-2" {
					(form_error())
					(volume_list(&base, &list, can_delete))
				}
			}))
			@if can_create {
				(card(&html! {
					(card_header("Add a volume", None))
					(card_content(&html! {
						form class="flex flex-col gap-4" "data-on:submit"=(add) {
							label class=(LABEL) {
								"Mount path"
								input class=(INPUT) type="text" required placeholder="/data" autocomplete="off" spellcheck="false" "data-bind:volume_path";
								span class="text-xs font-normal text-muted-foreground" { "An absolute path inside the container, such as /var/lib/app." }
							}
							div {
								button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Add volume" }
							}
						}
					}))
				}))
			}
		}
	};
	Ok(apps::shell(
		&s,
		&ctx,
		&project,
		&app,
		Tab::Volumes,
		&content,
	))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct VolumeSignals {
	volume_path: String,
}

async fn volume_add(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	ReadSignals(sig): ReadSignals<VolumeSignals>,
) -> Response {
	let result = async {
		volumes::create(
			&s.db,
			ctx.user.id,
			ctx.team.id,
			project,
			app,
			sig.volume_path.trim(),
		)
		.await?;
		volumes::list(&s.db, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(list) => {
			let base = apps::app_url(ctx.team.id, project, app);
			let can_delete = ctx.perms.can(Action::Delete, Resource::Volume);
			sse::patch(&volume_list(&base, &list, can_delete))
				.and(sse::patch(&saved_slot(true)))
				.and(sse::signals(&json!({ "volume_path": "", "_error": "" })))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn volume_remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, VolumeId)>,
) -> Response {
	let result = async {
		volumes::remove(&s, ctx.user.id, ctx.team.id, project, app, id).await?;
		volumes::list(&s.db, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(list) => {
			let base = apps::app_url(ctx.team.id, project, app);
			let can_delete = ctx.perms.can(Action::Delete, Resource::Volume);
			sse::patch(&volume_list(&base, &list, can_delete))
				.and(sse::patch(&saved_slot(true)))
				.and(sse::signals(&json!({ "_confirm": "", "_error": "" })))
				.into_response()
		}
		// Close the confirm box so the message is visible next to the list.
		Err(e) => {
			sse::signals(&json!({ "_confirm": "", "_error": error::message(&e) })).into_response()
		}
	}
}

#[cfg(test)]
mod tests {
	use std::cell::RefCell;

	use axum::Router;
	use axum::http::StatusCode;
	use serde_json::{Value, json};

	use crate::tests::{
		PgPool, application_id, call, count, db_test, env_var_ciphertext, env_var_id,
		local_server_id, only_id, project_id, rename_applications, revoke_permission,
	};
	use crate::ui::testing::{
		assert_contains, assert_not_contains, get_html, post_signals, ui_login, ui_session,
	};

	/// A value that must never come back out, whatever the page.
	const SECRET: &str = "S3CR3T-0b9a1c5e-6f0e-4d0e-9a52-7c1d2f3a4b5c";

	struct Fixture {
		app: Router,
		cookie: String,
		token: String,
		team: i64,
		project: i64,
		application: i64,
		/// Every response body this fixture received.
		seen: RefCell<Vec<String>>,
	}

	impl Fixture {
		async fn new(db: &PgPool) -> Self {
			let app = crate::tests::app(db.clone()).await;
			let (cookie, team) = ui_session(&app, "alice").await;
			post_signals(
				&app,
				&format!("/teams/{team}/projects"),
				Some(&cookie),
				&json!({ "name": "Shop", "slug": "shop" }),
			)
			.await;
			let project = project_id(db, "shop").await;
			let server = local_server_id(db, team).await;
			let (s, _, _) = post_signals(
				&app,
				&format!("/teams/{team}/projects/{project}/apps"),
				Some(&cookie),
				&json!({
					"name": "Web", "slug": "web", "server_id": server.to_string(),
					"repo_url": "https://example.com/web.git", "git_ref": "main",
					"deploy_key": "", "git_token": "", "build_strategy": "dockerfile",
					"dockerfile_path": "Dockerfile", "container_port": "3000",
					"memory_limit_mb": "", "cpu_limit": "",
				}),
			)
			.await;
			assert_eq!(s, StatusCode::OK);
			let application = application_id(db, "web").await;
			let creds =
				json!({ "email": "alice@example.com", "password": "correct horse battery" });
			let (_, login) = call(&app, "POST", "/api/v1/auth/login", None, Some(creds)).await;
			Self {
				app,
				cookie,
				token: login["token"].as_str().unwrap().to_owned(),
				team,
				project,
				application,
				seen: RefCell::new(Vec::new()),
			}
		}

		fn url(&self, tab: &str) -> String {
			format!(
				"/teams/{}/projects/{}/apps/{}/{tab}",
				self.team, self.project, self.application
			)
		}

		fn api(&self, tab: &str) -> String {
			format!(
				"/api/v1/teams/{}/projects/{}/applications/{}/{tab}",
				self.team, self.project, self.application
			)
		}

		async fn get(&self, uri: &str) -> (StatusCode, String) {
			let (s, _, body) = get_html(&self.app, uri, Some(&self.cookie)).await;
			self.seen.borrow_mut().push(body.clone());
			(s, body)
		}

		async fn page(&self, tab: &str) -> String {
			let (s, body) = self.get(&self.url(tab)).await;
			assert_eq!(s, StatusCode::OK, "{tab}: {body}");
			body
		}

		async fn post(&self, uri: &str, signals: &Value) -> String {
			let (_, _, body) = post_signals(&self.app, uri, Some(&self.cookie), signals).await;
			self.seen.borrow_mut().push(body.clone());
			body
		}

		async fn set_env(&self, key: &str, value: &str) -> String {
			self.post(
				&self.url("env"),
				&json!({ "env_key": key, "env_value": value }),
			)
			.await
		}

		async fn api_keys(&self) -> Vec<String> {
			let (s, list) = call(
				&self.app,
				"GET",
				&self.api("env-vars"),
				Some(&self.token),
				None,
			)
			.await;
			assert_eq!(s, StatusCode::OK);
			list.as_array()
				.unwrap()
				.iter()
				.map(|v| v["key"].as_str().unwrap().to_owned())
				.collect()
		}
	}

	/// Adds `name` to the fixture's team with the read-only Member role.
	async fn add_member(f: &Fixture, name: &str) -> String {
		user_with_team_quiet(&f.app, name).await;
		let (s, _) = call(
			&f.app,
			"POST",
			&format!("/api/v1/teams/{}/members", f.team),
			Some(&f.token),
			Some(json!({ "email": format!("{name}@example.com"), "role_id": 2 })),
		)
		.await;
		assert!(s.is_success(), "add member: {s}");
		ui_login(&f.app, name).await
	}

	async fn user_with_team_quiet(app: &Router, name: &str) {
		crate::tests::user_with_team(app, name).await;
	}

	// --- environment variables ---------------------------------------------------

	#[db_test]
	async fn an_env_value_never_appears_in_any_response(db: PgPool) {
		let f = Fixture::new(&db).await;
		let (a, b) = (format!("{SECRET}-a"), format!("{SECRET}-b"));
		let body = f.set_env("API_TOKEN", &a).await;
		assert_contains(&body, "API_TOKEN");
		assert_contains(&body, "Saved. Changes apply on the next deploy.");
		f.set_env("API_TOKEN", &b).await;
		f.post(
			&f.url("env/bulk"),
			&json!({ "env_bulk": format!("# c\nBULK_ONE={b}\nnope {b}\nexport BULK_TWO=\"{a}\"\n") }),
		)
		.await;
		let page = f.page("env").await;
		assert_contains(&page, "API_TOKEN");
		assert_contains(&page, "BULK_TWO");
		assert_contains(&page, "Value hidden");
		for uri in [
			f.url(""),
			f.url("settings"),
			f.url("domains"),
			f.url("volumes"),
			format!("/teams/{}/projects/{}", f.team, f.project),
		] {
			f.get(uri.trim_end_matches('/')).await;
		}
		// The value reached the database encrypted, and the JSON API agrees.
		let stored = env_var_ciphertext(&db, "API_TOKEN").await;
		assert!(!String::from_utf8_lossy(&stored).contains(SECRET));
		let (_, list) = call(&f.app, "GET", &f.api("env-vars"), Some(&f.token), None).await;
		f.seen.borrow_mut().push(list.to_string());

		let seen = f.seen.borrow();
		assert!(seen.len() >= 9);
		for body in seen.iter() {
			assert_not_contains(body, SECRET);
		}
	}

	#[db_test]
	async fn setting_a_key_again_replaces_it_and_delete_removes_it(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.set_env("MODE", "one").await;
		f.set_env("MODE", "two").await;
		f.set_env("OTHER", "x").await;
		assert_eq!(f.api_keys().await, ["MODE", "OTHER"]);
		assert_eq!(count(&db, "env_vars").await, 2);

		let id = env_var_id(&db, "MODE").await;
		let body = f
			.post(&f.url(&format!("env/{id}/delete")), &json!({}))
			.await;
		assert_contains(&body, "Saved. Changes apply on the next deploy.");
		assert_not_contains(&body, "MODE");
		assert_contains(&body, "OTHER");
		assert_eq!(f.api_keys().await, ["OTHER"]);
		let body = f
			.post(&f.url(&format!("env/{id}/delete")), &json!({}))
			.await;
		assert_contains(&body, "That no longer exists.");
	}

	#[db_test]
	async fn a_bad_variable_name_is_reported_inline_and_saves_nothing(db: PgPool) {
		let f = Fixture::new(&db).await;
		for key in ["1ABC", "A B", "", "A-B"] {
			let body = f.set_env(key, "v").await;
			assert_contains(&body, "Name must be letters, digits and underscores");
			assert_not_contains(&body, "Saved.");
		}
		assert_eq!(count(&db, "env_vars").await, 0);
	}

	#[db_test]
	async fn a_bulk_paste_saves_the_good_lines_and_names_the_bad_one(db: PgPool) {
		let f = Fixture::new(&db).await;
		let paste = format!(
			"# my app\n\nFIRST=1\nthis line has no equals {SECRET}\nSECOND=\"two words\"\nTHIRD={SECRET} # note\n"
		);
		let body = f
			.post(&f.url("env/bulk"), &json!({ "env_bulk": paste }))
			.await;
		assert_contains(&body, "Saved 3 variables.");
		assert_contains(&body, "Line 4: expected KEY=VALUE.");
		assert_not_contains(&body, "Line 3");
		assert_not_contains(&body, "Line 5");
		assert_not_contains(&body, SECRET);
		// The text stays in the box so the line can be fixed.
		assert_not_contains(&body, r#""env_bulk":"""#);
		assert_contains(&body, "Saved. Changes apply on the next deploy.");
		assert_eq!(f.api_keys().await, ["FIRST", "SECOND", "THIRD"]);

		// A clean paste clears the box.
		let body = f
			.post(&f.url("env/bulk"), &json!({ "env_bulk": "FOURTH=4" }))
			.await;
		assert_contains(&body, "Saved 1 variable.");
		assert_contains(&body, "env_bulk");
		assert_not_contains(&body, "Line ");
	}

	#[db_test]
	async fn a_paste_with_only_bad_lines_saves_nothing_and_shows_no_banner(db: PgPool) {
		let f = Fixture::new(&db).await;
		let body = f
			.post(&f.url("env/bulk"), &json!({ "env_bulk": "bad\n2X=1" }))
			.await;
		assert_contains(&body, "Saved 0 variables.");
		assert_contains(&body, "Line 1");
		assert_contains(&body, "Line 2");
		assert_not_contains(&body, "Changes apply");
		assert_eq!(count(&db, "env_vars").await, 0);

		let big = "A=1\n".repeat(501);
		let body = f
			.post(&f.url("env/bulk"), &json!({ "env_bulk": big }))
			.await;
		assert_contains(&body, "Paste at most 500 lines");
		assert_eq!(count(&db, "env_vars").await, 0);
	}

	// --- domains -----------------------------------------------------------------

	#[db_test]
	async fn domains_are_added_listed_and_removed_with_dns_help(db: PgPool) {
		let f = Fixture::new(&db).await;
		let page = f.page("domains").await;
		assert_contains(&page, "No custom domains yet.");
		assert_contains(&page, "DNS setup");
		// The local test server is reached as a name, so a CNAME to it.
		assert_contains(&page, "<strong>CNAME</strong> record");
		assert_contains(&page, "localhost");

		let body = f
			.post(
				&f.url("domains"),
				&json!({ "domain_hostname": "  App.Example.com " }),
			)
			.await;
		assert_contains(&body, "app.example.com");
		assert_contains(&body, "Saved. Changes apply on the next deploy.");
		let body = f
			.post(
				&f.url("domains"),
				&json!({ "domain_hostname": "not a host" }),
			)
			.await;
		assert_contains(&body, "invalid hostname");

		let id = only_id(&db, "custom_domains").await;
		let body = f
			.post(&f.url(&format!("domains/{id}/delete")), &json!({}))
			.await;
		assert_not_contains(&body, "app.example.com");
		assert_contains(&body, "No custom domains yet.");
		assert_eq!(count(&db, "custom_domains").await, 0);
	}

	#[db_test]
	async fn a_taken_hostname_is_a_conflict_shown_inline(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.post(
			&f.url("domains"),
			&json!({ "domain_hostname": "a.example.com" }),
		)
		.await;
		let body = f
			.post(
				&f.url("domains"),
				&json!({ "domain_hostname": "A.example.com" }),
			)
			.await;
		assert_contains(&body, "already routed to another application");
		assert_not_contains(&body, "Saved.");
		assert_eq!(count(&db, "custom_domains").await, 1);
	}

	#[db_test]
	async fn the_https_banner_follows_the_acme_email(db: PgPool) {
		let f = Fixture::new(&db).await;
		let page = f.page("domains").await;
		assert_contains(&page, "HTTPS is off for custom domains.");

		let mut state = crate::tests::state(db.clone());
		state.config = std::sync::Arc::new(crate::state::Config {
			acme_email: Some("ops@example.com".to_owned()),
			..(*state.config).clone()
		});
		let app = crate::routes::routes(state).await.unwrap();
		let (s, _, page) = get_html(&app, &f.url("domains"), Some(&f.cookie)).await;
		assert_eq!(s, StatusCode::OK);
		assert_not_contains(&page, "HTTPS is off");
		assert_contains(&page, "DNS setup");
	}

	#[test]
	fn dns_help_asks_for_an_a_record_when_the_server_is_an_ip() {
		let ip = super::dns_help("203.0.113.7").into_string();
		assert!(ip.contains("<strong>A</strong> record"), "{ip}");
		assert!(ip.contains("203.0.113.7"));
		assert!(!ip.contains("CNAME"));
		let name = super::dns_help("box.example.com").into_string();
		assert!(name.contains("<strong>CNAME</strong> record"), "{name}");
	}

	// --- volumes -----------------------------------------------------------------

	#[db_test]
	async fn volumes_are_added_and_a_duplicate_path_is_a_conflict(db: PgPool) {
		let f = Fixture::new(&db).await;
		let page = f.page("volumes").await;
		assert_contains(&page, "No volumes yet.");

		let body = f
			.post(&f.url("volumes"), &json!({ "volume_path": " /var/data " }))
			.await;
		assert_contains(&body, "/var/data");
		assert_contains(&body, "podkit-vol-");
		assert_contains(&body, "Saved. Changes apply on the next deploy.");
		let body = f
			.post(&f.url("volumes"), &json!({ "volume_path": "/var/data" }))
			.await;
		assert_contains(&body, "a volume is already mounted at that path");
		assert_not_contains(&body, "Saved.");
		let body = f
			.post(&f.url("volumes"), &json!({ "volume_path": "relative" }))
			.await;
		assert_contains(&body, "absolute path");
		assert_eq!(count(&db, "volumes").await, 1);
	}

	#[db_test]
	async fn deleting_a_volume_asks_first_and_survives_an_unreachable_agent(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.post(&f.url("volumes"), &json!({ "volume_path": "/data" }))
			.await;
		let page = f.page("volumes").await;
		let id = only_id(&db, "volumes").await;
		// The confirm box is part of the page and hidden until asked for.
		assert_contains(&page, &format!("$_confirm = '{id}'"));
		assert_contains(&page, "permanently deletes all data in /data");
		assert_contains(&page, "can't be recovered");
		assert_contains(&page, &format!("/volumes/{id}/delete"));

		// No agent runs in tests: the failure is reported, the row stays.
		let body = f
			.post(&f.url(&format!("volumes/{id}/delete")), &json!({}))
			.await;
		assert_not_contains(&body, "Saved.");
		assert_contains(&body, "_error");
		assert_eq!(count(&db, "volumes").await, 1);
	}

	// --- scope, permissions and escaping --------------------------------------------

	#[db_test]
	async fn another_teams_app_is_404_on_every_tab_and_action(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.set_env("KEEP", "v").await;
		f.post(
			&f.url("domains"),
			&json!({ "domain_hostname": "keep.example.com" }),
		)
		.await;
		f.post(&f.url("volumes"), &json!({ "volume_path": "/keep" }))
			.await;
		let (_, bob_team) = ui_session(&f.app, "bob").await;
		let bob = ui_login(&f.app, "bob").await;

		// Alice's team, and Bob's own team with Alice's ids.
		for team in [f.team, bob_team] {
			for tab in ["env", "domains", "volumes"] {
				let uri = format!(
					"/teams/{team}/projects/{}/apps/{}/{tab}",
					f.project, f.application
				);
				let (s, _, body) = get_html(&f.app, &uri, Some(&bob)).await;
				assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
				assert_contains(&body, "Not found");
				assert_not_contains(&body, "KEEP");
				assert_not_contains(&body, "keep.example.com");
				assert_not_contains(&body, "/keep");

				let (_, _, body) = post_signals(
					&f.app,
					&uri,
					Some(&bob),
					&json!({
						"env_key": "EVIL", "env_value": "x", "domain_hostname": "evil.example.com",
						"volume_path": "/evil",
					}),
				)
				.await;
				assert_contains(&body, "That no longer exists.");
			}
			let uri = format!(
				"/teams/{team}/projects/{}/apps/{}/env/bulk",
				f.project, f.application
			);
			let (_, _, body) =
				post_signals(&f.app, &uri, Some(&bob), &json!({ "env_bulk": "EVIL=1" })).await;
			assert_contains(&body, "That no longer exists.");
		}
		assert_eq!(count(&db, "env_vars").await, 1);
		assert_eq!(count(&db, "custom_domains").await, 1);
		assert_eq!(count(&db, "volumes").await, 1);
	}

	#[db_test]
	async fn each_resource_is_authorized_on_its_own(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.set_env("KEEP", "v").await;
		f.post(
			&f.url("domains"),
			&json!({ "domain_hostname": "keep.example.com" }),
		)
		.await;
		f.post(&f.url("volumes"), &json!({ "volume_path": "/keep" }))
			.await;
		let carol = add_member(&f, "carol").await;
		let env_id = only_id(&db, "env_vars").await;

		// A read-only member sees each tab, without its forms or buttons.
		for (tab, hidden) in [
			("env", "Save variable"),
			("domains", "Add domain"),
			("volumes", "Add volume"),
		] {
			let (s, _, page) = get_html(&f.app, &f.url(tab), Some(&carol)).await;
			assert_eq!(s, StatusCode::OK, "{tab}");
			assert_not_contains(&page, hidden);
			assert_not_contains(&page, "/delete");
		}
		// Forged posts are refused, for each resource.
		for (uri, signals) in [
			(f.url("env"), json!({ "env_key": "X", "env_value": "1" })),
			(f.url("env/bulk"), json!({ "env_bulk": "X=1" })),
			(f.url(&format!("env/{env_id}/delete")), json!({})),
			(
				f.url("domains"),
				json!({ "domain_hostname": "x.example.com" }),
			),
			(f.url("volumes"), json!({ "volume_path": "/x" })),
		] {
			let (_, _, body) = post_signals(&f.app, &uri, Some(&carol), &signals).await;
			assert!(body.contains("permission"), "{uri} not refused: {body}");
		}
		assert_eq!(count(&db, "env_vars").await, 1);
		assert_eq!(count(&db, "custom_domains").await, 1);
		assert_eq!(count(&db, "volumes").await, 1);

		// Taking away one read permission closes that tab only.
		revoke_permission(&db, 2, "read", "env_var").await;
		let (s, _, page) = get_html(&f.app, &f.url("env"), Some(&carol)).await;
		assert_eq!(s, StatusCode::FORBIDDEN);
		assert_not_contains(&page, "KEEP");
		let (s, _, page) = get_html(&f.app, &f.url("domains"), Some(&carol)).await;
		assert_eq!(s, StatusCode::OK);
		assert_not_contains(&page, "/env\"");
		assert_contains(&page, "/domains\"");
		let (s, _, _) = get_html(&f.app, &f.url("volumes"), Some(&carol)).await;
		assert_eq!(s, StatusCode::OK);

		// And a missing create on the owner's role blocks that resource alone.
		revoke_permission(&db, 1, "create", "domain").await;
		let body = f
			.post(
				&f.url("domains"),
				&json!({ "domain_hostname": "y.example.com" }),
			)
			.await;
		assert_contains(&body, "permission");
		let body = f.set_env("STILL_OK", "1").await;
		assert_contains(&body, "STILL_OK");
		let body = f
			.post(&f.url("volumes"), &json!({ "volume_path": "/still-ok" }))
			.await;
		assert_contains(&body, "/still-ok");
		revoke_permission(&db, 1, "delete", "volume").await;
		let page = f.page("volumes").await;
		assert_not_contains(&page, "/delete");
		let page = f.page("env").await;
		assert_contains(&page, "/delete");
	}

	#[db_test]
	async fn names_are_escaped_on_the_config_tabs(db: PgPool) {
		let f = Fixture::new(&db).await;
		let name = "<script>alert(1)</script>";
		rename_applications(&db, name).await;
		for tab in ["env", "domains", "volumes"] {
			let page = f.page(tab).await;
			assert_not_contains(&page, name);
			assert_contains(&page, "&lt;script&gt;alert(1)&lt;/script&gt;");
		}
	}
}
