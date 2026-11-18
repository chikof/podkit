//! Application pages: create (with the one-time webhook secret), overview,
//! settings and delete.
//!
//! Credentials are write-only. The forms send them to the server; nothing
//! here puts a stored deploy key or token, or the webhook secret after the
//! create response, into HTML or a signal.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use crate::applications::BuildStrategy;
use crate::applications::service::{self, AppInput, ApplicationView, Created};
use crate::auth::permission::{Action, Resource};
use crate::error::ServerError;
use crate::ids::{ApplicationId, ProjectId, ServerId, TeamId};
use crate::projects;
use crate::projects::Project;
use crate::servers::{Server, ServerStatus};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, BUTTON_DANGER, BUTTON_OUTLINE, INPUT, LABEL, TAB, TAB_ACTIVE, TEXTAREA, card,
	card_content, card_header,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page, form_error};
use crate::ui::status::{Status, status_badge};
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/projects/{project_id}/apps", post(create))
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/new",
			get(new_page),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}",
			get(overview),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/settings",
			get(settings_page).post(save),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/delete",
			post(remove),
		)
}

fn project_url(team: TeamId, project: ProjectId) -> String {
	format!("/teams/{team}/projects/{project}")
}

pub fn app_url(team: TeamId, project: ProjectId, app: ApplicationId) -> String {
	format!("/teams/{team}/projects/{project}/apps/{app}")
}

/// Where the application answers: its generated hostname on the ingress port.
pub fn public_link(s: &AppState, host: &str) -> String {
	match s.config.ingress_port {
		80 => format!("http://{host}"),
		port => format!("http://{host}:{port}"),
	}
}

/// The site the browser used, e.g. `https://podkit.example`: the proxy's
/// forwarded host and protocol when it sets them, else `Host` over plain HTTP.
fn origin(headers: &HeaderMap) -> String {
	let first = |name: &str| {
		headers
			.get(name)
			.and_then(|v| v.to_str().ok())
			.and_then(|v| v.split(',').next())
			.map(str::trim)
			.filter(|v| !v.is_empty())
	};
	let proto = match first("x-forwarded-proto") {
		Some("https") => "https",
		_ => "http",
	};
	let host = first("x-forwarded-host")
		.or_else(|| first(header::HOST.as_str()))
		.unwrap_or("localhost");
	format!("{proto}://{host}")
}

pub(super) fn crumbs<'a>(ctx: &TeamCtx, project: &'a Project, last: &'a str) -> Vec<Crumb<'a>> {
	vec![
		("Projects", Some(format!("/teams/{}/projects", ctx.team.id))),
		(
			project.name.as_str(),
			Some(project_url(ctx.team.id, project.id)),
		),
		(last, None),
	]
}

// --- shared form -----------------------------------------------------------

/// Everything the create and settings forms send, as strings: a number input
/// would come back from Datastar as a number or a string depending on use.
#[derive(Deserialize, Default)]
#[serde(default)]
struct AppSignals {
	name: String,
	slug: String,
	server_id: String,
	repo_url: String,
	git_ref: String,
	deploy_key: String,
	git_token: String,
	build_strategy: String,
	dockerfile_path: String,
	container_port: String,
	memory_limit_mb: String,
	cpu_limit: String,
}

fn optional<T: std::str::FromStr>(raw: &str, what: &str) -> Result<Option<T>, ServerError> {
	let raw = raw.trim();
	if raw.is_empty() {
		return Ok(None);
	}
	raw.parse()
		.map(Some)
		.map_err(|_| ServerError::Validation(format!("{what} must be a number.")))
}

impl AppSignals {
	fn input(&self) -> Result<AppInput, ServerError> {
		let container_port = optional::<i32>(&self.container_port, "Container port")?
			.ok_or_else(|| ServerError::Validation("Container port is required.".to_owned()))?;
		let or = |value: &str, default: &str| {
			let value = value.trim();
			if value.is_empty() { default } else { value }.to_owned()
		};
		let secret = |value: &str| (!value.is_empty()).then(|| value.to_owned());
		Ok(AppInput {
			name: self.name.clone(),
			slug: self.slug.trim().to_owned(),
			repo_url: self.repo_url.trim().to_owned(),
			git_ref: or(&self.git_ref, "main"),
			deploy_key: secret(&self.deploy_key),
			git_token: secret(self.git_token.trim()),
			build_strategy: or(&self.build_strategy, "dockerfile"),
			dockerfile_path: or(&self.dockerfile_path, "Dockerfile"),
			container_port,
			memory_limit_mb: optional(&self.memory_limit_mb, "Memory limit")?,
			cpu_limit: optional(&self.cpu_limit, "CPU limit")?,
			environment_id: None,
		})
	}
}

const NO_CREDENTIAL: &str =
	"Only for private repositories. It is stored encrypted and never shown again.";

/// What a stored deploy key looks like to the user: whether there is one,
/// never what it is. Patched in place after a save.
fn deploy_key_hint(app: Option<&ApplicationView>) -> Markup {
	html! {
		p id="deploy-key-hint" class="text-xs font-normal text-muted-foreground" {
			@if app.is_some_and(|a| a.has_deploy_key) {
				"A deploy key is stored. Leave this empty to keep it; paste a new one to replace it."
			} @else { (NO_CREDENTIAL) }
		}
	}
}

/// Same for the git access token.
fn git_token_hint(app: Option<&ApplicationView>) -> Markup {
	html! {
		p id="git-token-hint" class="text-xs font-normal text-muted-foreground" {
			@if app.is_some_and(|a| a.has_git_token) {
				"A token is stored. Leave this empty to keep it; enter a new one to replace it."
			} @else { (NO_CREDENTIAL) }
		}
	}
}

/// The fields of `CreateApplicationRequest`. With `servers` the form picks
/// where to run; without it (settings) the server can't change.
fn fields(servers: Option<&[Server]>, app: Option<&ApplicationView>) -> Markup {
	html! {
		label class=(LABEL) {
			"Name"
			input class=(INPUT) type="text" required maxlength="60" "data-bind:name";
		}
		label class=(LABEL) {
			"Slug"
			input class=(INPUT) type="text" required maxlength="60" placeholder="my-app" "data-bind:slug";
			span class="text-xs font-normal text-muted-foreground" { "Part of the app's public hostname. Unique per server." }
		}
		@if let Some(servers) = servers {
			label class=(LABEL) {
				"Server"
				select class=(INPUT) required "data-bind:server_id" {
					@for server in servers {
						option value=(server.id.0) disabled[server.status != ServerStatus::Active] {
							(server.name)
							@if server.status != ServerStatus::Active { " (" (server.status.label()) ")" }
						}
					}
				}
				@if !servers.iter().any(|s| s.status == ServerStatus::Active) {
					span class="text-xs font-normal text-destructive" { "No server is active, so nothing can be deployed yet. Check the Servers page." }
				}
			}
		}
		label class=(LABEL) {
			"Repository URL"
			input class=(INPUT) type="text" required placeholder="https://github.com/you/app.git or git@github.com:you/app.git" autocomplete="off" spellcheck="false" "data-bind:repo_url";
		}
		label class=(LABEL) {
			"Branch, tag or ref"
			input class=(INPUT) type="text" placeholder="main" autocomplete="off" spellcheck="false" "data-bind:git_ref";
		}
		label class=(LABEL) "data-show"="$repo_url !== '' && !$repo_url.startsWith('https://')" style="display: none" {
			"Deploy key (SSH private key)"
			textarea class=(TEXTAREA) rows="5" autocomplete="off" spellcheck="false" placeholder="Paste the private key" "data-bind:deploy_key" {}
			(deploy_key_hint(app))
		}
		label class=(LABEL) "data-show"="$repo_url.startsWith('https://')" style="display: none" {
			"Access token"
			input class=(INPUT) type="password" autocomplete="off" placeholder="Only for private repositories" "data-bind:git_token";
			(git_token_hint(app))
		}
		label class=(LABEL) {
			"Build strategy"
			select class=(INPUT) "data-bind:build_strategy" {
				option value="dockerfile" { "Dockerfile" }
				option value="nixpacks" { "Nixpacks (detect automatically)" }
			}
		}
		label class=(LABEL) "data-show"="$build_strategy === 'dockerfile'" {
			"Dockerfile path"
			input class=(INPUT) type="text" placeholder="Dockerfile" autocomplete="off" spellcheck="false" "data-bind:dockerfile_path";
		}
		label class=(LABEL) {
			"Container port"
			input class=(INPUT) type="text" inputmode="numeric" required placeholder="3000" "data-bind:container_port";
		}
		div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
			label class=(LABEL) {
				"Memory limit (MB)"
				input class=(INPUT) type="text" inputmode="numeric" placeholder="No limit" "data-bind:memory_limit_mb";
			}
			label class=(LABEL) {
				"CPU limit (cores)"
				input class=(INPUT) type="text" inputmode="decimal" placeholder="No limit" "data-bind:cpu_limit";
			}
		}
	}
}

fn blank_signals(servers: &[Server]) -> String {
	let server = servers
		.iter()
		.find(|s| s.status == ServerStatus::Active)
		.map(|s| s.id.to_string())
		.unwrap_or_default();
	json!({
		"name": "", "slug": "", "server_id": server, "repo_url": "", "git_ref": "main",
		"deploy_key": "", "git_token": "", "build_strategy": "dockerfile",
		"dockerfile_path": "Dockerfile", "container_port": "", "memory_limit_mb": "",
		"cpu_limit": "", "_error": "",
	})
	.to_string()
}

fn prefilled_signals(app: &ApplicationView) -> String {
	json!({
		"name": app.name, "slug": app.slug, "repo_url": app.repo_url, "git_ref": app.git_ref,
		"deploy_key": "", "git_token": "", "build_strategy": app.build_strategy.as_str(),
		"dockerfile_path": app.dockerfile_path, "container_port": app.container_port.to_string(),
		"memory_limit_mb": app.memory_limit_mb.map(|m| m.to_string()).unwrap_or_default(),
		"cpu_limit": app.cpu_limit.map(|c| c.to_string()).unwrap_or_default(),
		"confirm": "", "_error": "", "_delete_error": "",
	})
	.to_string()
}

/// The signals that hold credentials, emptied after a request that took them.
fn clear_credentials() -> serde_json::Value {
	json!({ "deploy_key": "", "git_token": "" })
}

// --- create ----------------------------------------------------------------

async fn new_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
) -> Result<Markup, Response> {
	let fail = |e: ServerError| error::page(&ctx, &e);
	let project = projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	let servers = service::servers_for_create(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(fail)?;

	let post = format!(
		"evt.preventDefault(); @post('{}/apps')",
		project_url(ctx.team.id, project.id)
	);
	let body = html! {
		div class="flex flex-col gap-6" data-signals=(blank_signals(&servers)) {
			div id="app-form" {
				(card(&html! {
					(card_header("New application", Some("A git repository, built and run as a container on one of your servers.")))
					(card_content(&html! {
						form class="flex flex-col gap-4" "data-on:submit"=(post) {
							(fields(Some(&servers), None))
							(form_error())
							div {
								button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Create application" }
							}
						}
					}))
				}))
			}
		}
	};
	Ok(app_page(
		&ctx,
		"New application",
		&crumbs(&ctx, &project, "New application"),
		&body,
	))
}

/// A read-only field with a button that copies its value. `execCommand`
/// rather than the Clipboard API, which browsers withhold over plain HTTP.
fn copy_row(label: &str, value: &str) -> Markup {
	html! {
		div class="flex flex-col gap-2 text-sm font-medium" {
			span { (label) }
			div class="flex gap-2" {
				input class=(INPUT) type="text" readonly value=(value) autocomplete="off" spellcheck="false" "data-on:focus"="el.select()";
				button type="button" class=(BUTTON_OUTLINE) "data-on:click"="el.previousElementSibling.select(); document.execCommand('copy')" { "Copy" }
			}
		}
	}
}

/// Shown once, in the response to the create request. Nothing here can be
/// fetched again: the secret exists in plaintext only in this stream.
fn created_card(ctx: &TeamCtx, origin: &str, created: &Created) -> Markup {
	let app = &created.app;
	let webhook = format!("{origin}/webhooks/deploy/{}", app.id);
	html! {
		div id="app-form" {
			(card(&html! {
				(card_header("Application created", Some("Add the webhook to your git provider to deploy on every push.")))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						(copy_row("Webhook URL", &webhook))
						(copy_row("Webhook secret", &created.webhook_secret))
						p class="text-sm text-destructive" { "Copy the secret now. It is shown only this once and can't be recovered." }
						p class="text-sm text-muted-foreground" {
							"Send it as the GitHub signature secret, or in an "
							code { "X-Gitlab-Token" } " or " code { "X-Webhook-Secret" } " header."
						}
						div {
							a class=(BUTTON) href=(app_url(ctx.team.id, app.project_id, app.id)) { "Go to " (app.name) }
						}
					}
				}))
			}))
		}
	}
}

async fn create(
	State(s): State<AppState>,
	ctx: TeamCtx,
	headers: HeaderMap,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<AppSignals>,
) -> Response {
	let result = async {
		let server = sig
			.server_id
			.parse::<i64>()
			.map(ServerId)
			.map_err(|_| ServerError::Validation("Pick a server.".to_owned()))?;
		let input = sig.input()?;
		service::create(&s, ctx.user.id, ctx.team.id, project, server, input).await
	}
	.await;
	match result {
		Ok(created) => sse::patch(&created_card(&ctx, &origin(&headers), &created))
			.and(sse::signals(&clear_credentials()))
			.and(sse::signals(&json!({ "_error": "" })))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

// --- overview and settings ---------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
	Overview,
	Deployments,
	Logs,
	Env,
	Domains,
	Volumes,
	Settings,
}

impl Tab {
	fn label(self) -> &'static str {
		match self {
			Self::Overview => "Overview",
			Self::Deployments => "Deployments",
			Self::Logs => "Logs",
			Self::Env => "Environment",
			Self::Domains => "Domains",
			Self::Volumes => "Volumes",
			Self::Settings => "Settings",
		}
	}
}

fn header_card(s: &AppState, app: &ApplicationView) -> Markup {
	let link = public_link(s, &app.url);
	html! {
		div id="app-header" class="flex flex-col gap-1" {
			h1 class="text-xl font-semibold" { (app.name) }
			a class="text-sm text-muted-foreground hover:underline" href=(link) target="_blank" rel="noopener noreferrer" { (link) }
			p class="text-sm text-muted-foreground break-all" { (app.repo_url) "@" (app.git_ref) }
		}
	}
}

fn tabs(ctx: &TeamCtx, app: &ApplicationView, active: Tab) -> Markup {
	let base = app_url(ctx.team.id, app.project_id, app.id);
	let class = |tab| if tab == active { TAB_ACTIVE } else { TAB };
	let can_configure = ctx.perms.can(Action::Update, Resource::Application)
		|| ctx.perms.can(Action::Delete, Resource::Application);
	html! {
		nav class="flex gap-1 overflow-x-auto border-b border-border" aria-label="Application" {
			a class=(class(Tab::Overview)) href=(base) { (Tab::Overview.label()) }
			@if ctx.perms.can(Action::Read, Resource::Deployment) {
				a class=(class(Tab::Deployments)) href={ (base) "/deployments" } { (Tab::Deployments.label()) }
			}
			@if ctx.perms.can(Action::Read, Resource::Application) {
				a class=(class(Tab::Logs)) href={ (base) "/logs" } { (Tab::Logs.label()) }
			}
			@if ctx.perms.can(Action::Read, Resource::EnvVar) {
				a class=(class(Tab::Env)) href={ (base) "/env" } { (Tab::Env.label()) }
			}
			@if ctx.perms.can(Action::Read, Resource::Domain) {
				a class=(class(Tab::Domains)) href={ (base) "/domains" } { (Tab::Domains.label()) }
			}
			@if ctx.perms.can(Action::Read, Resource::Volume) {
				a class=(class(Tab::Volumes)) href={ (base) "/volumes" } { (Tab::Volumes.label()) }
			}
			@if can_configure {
				a class=(class(Tab::Settings)) href={ (base) "/settings" } { (Tab::Settings.label()) }
			}
		}
	}
}

pub(super) fn shell(
	s: &AppState,
	ctx: &TeamCtx,
	project: &Project,
	app: &ApplicationView,
	active: Tab,
	content: &Markup,
) -> Markup {
	let body = html! {
		(header_card(s, app))
		(super::deployments::actions(ctx, app))
		(tabs(ctx, app, active))
		(content)
	};
	let title = match active {
		Tab::Overview => app.name.clone(),
		tab => format!("{} · {}", tab.label(), app.name),
	};
	app_page(ctx, &title, &crumbs(ctx, project, &app.name), &body)
}

pub(super) async fn load(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	app: ApplicationId,
) -> Result<(Project, ApplicationView), Response> {
	let fail = |e: ServerError| error::page(ctx, &e);
	let project = projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	let app = service::get(&s.db, ctx.user.id, ctx.team.id, project.id, app)
		.await
		.map_err(fail)?;
	Ok((project, app))
}

fn detail(label: &str, value: &Markup) -> Markup {
	html! {
		div class="flex flex-col gap-1" {
			dt class="text-muted-foreground" { (label) }
			dd class="font-medium break-words" { (value) }
		}
	}
}

async fn overview(
	State(s): State<AppState>,
	ctx: TeamCtx,
	headers: HeaderMap,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	let (project, app) = load(&s, &ctx, project, app).await?;
	let webhook = format!("{}/webhooks/deploy/{}", origin(&headers), app.id);
	let content = card(&html! {
		(card_header("Overview", None))
		(card_content(&html! {
			dl class="grid grid-cols-1 gap-x-6 gap-y-4 text-sm sm:grid-cols-2" {
				(detail("Latest deployment", &html! {
					@if let Some(status) = app.latest { (status_badge(status)) } @else { span class="text-muted-foreground" { "Not deployed yet" } }
				}))
				(detail("Server", &html! { (app.server_name) }))
				(detail("Repository", &html! { (app.repo_url) }))
				(detail("Ref", &html! { (app.git_ref) }))
				(detail("Build", &html! {
					@match app.build_strategy {
						BuildStrategy::Dockerfile => { "Dockerfile (" (app.dockerfile_path) ")" }
						BuildStrategy::Nixpacks => { "Nixpacks" }
					}
				}))
				(detail("Container port", &html! { (app.container_port) }))
				(detail("Memory limit", &html! {
					@if let Some(mb) = app.memory_limit_mb { (mb) " MB" } @else { "No limit" }
				}))
				(detail("CPU limit", &html! {
					@if let Some(cpu) = app.cpu_limit { (cpu) " cores" } @else { "No limit" }
				}))
				(detail("Source access", &html! {
					@if app.has_deploy_key { "Deploy key stored" }
					@else if app.has_git_token { "Access token stored" }
					@else { "Public repository" }
				}))
				(detail("Webhook URL", &html! { (webhook) }))
			}
		}))
	});
	Ok(shell(&s, &ctx, &project, &app, Tab::Overview, &content))
}

fn delete_error() -> Markup {
	html! {
		p class="text-sm text-destructive" role="alert" "data-show"="$_delete_error" "data-text"="$_delete_error" style="display: none" {}
	}
}

async fn settings_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	let can_update = ctx.perms.can(Action::Update, Resource::Application);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Application);
	if !can_update && !can_delete {
		return Err(error::page(&ctx, &ServerError::Forbidden));
	}
	let (project, app) = load(&s, &ctx, project, app).await?;
	let base = app_url(ctx.team.id, project.id, app.id);
	let save = format!("evt.preventDefault(); @post('{base}/settings')");
	let delete = format!("@post('{base}/delete')");

	let content = html! {
		div class="flex flex-col gap-6" data-signals=(prefilled_signals(&app)) {
			@if can_update {
				(card(&html! {
					(card_header("Settings", Some("Changes apply on the next deploy. The server can't be changed.")))
					(card_content(&html! {
						form class="flex flex-col gap-4" "data-on:submit"=(save) {
							(fields(None, Some(&app)))
							(form_error())
							div {
								button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Save changes" }
							}
						}
					}))
				}))
			}
			@if can_delete {
				(card(&html! {
					(card_header("Danger zone", None))
					(card_content(&html! {
						div class="flex flex-col gap-4" {
							p class="text-sm" {
								"Deleting stops and removes the application's containers and volumes. Type "
								strong { (app.slug) }
								" to confirm."
							}
							input class=(INPUT) type="text" autocomplete="off" aria-label="Application slug" "data-bind:confirm";
							(delete_error())
							div {
								button type="button" class=(BUTTON_DANGER) "data-on:click"=(delete) "data-indicator:_deleting" "data-attr:disabled"="$_deleting" { "Delete application" }
							}
						}
					}))
				}))
			}
		}
	};
	Ok(shell(&s, &ctx, &project, &app, Tab::Settings, &content))
}

async fn save(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	ReadSignals(sig): ReadSignals<AppSignals>,
) -> Response {
	let result = async {
		let input = sig.input()?;
		service::update(&s, ctx.user.id, ctx.team.id, project, app, input).await
	}
	.await;
	match result {
		Ok(updated) => sse::patch(&header_card(&s, &updated))
			.and(sse::patch(&deploy_key_hint(Some(&updated))))
			.and(sse::patch(&git_token_hint(Some(&updated))))
			.and(sse::signals(&clear_credentials()))
			.and(sse::signals(&json!({ "_error": "" })))
			.and(sse::toast("Settings saved"))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DeleteSignals {
	confirm: String,
}

async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	ReadSignals(sig): ReadSignals<DeleteSignals>,
) -> Response {
	let result = async {
		// Check the slug after a read, so a mistyped one never reaches the
		// delete; `remove` still authorizes the delete itself.
		let found = service::get(&s.db, ctx.user.id, ctx.team.id, project, app).await?;
		if sig.confirm != found.slug {
			return Ok(false);
		}
		service::remove(&s, ctx.user.id, ctx.team.id, project, app).await?;
		Ok::<_, ServerError>(true)
	}
	.await;
	match result {
		Ok(true) => sse::redirect(&project_url(ctx.team.id, project)).into_response(),
		Ok(false) => sse::signals(&json!({
			"_delete_error": "The slug doesn't match. Type it exactly to confirm.",
		}))
		.into_response(),
		Err(e) => sse::signals(&json!({ "_delete_error": error::message(&e) })).into_response(),
	}
}

#[cfg(test)]
mod tests {
	use axum::Router;
	use axum::http::StatusCode;
	use serde_json::{Value, json};

	use crate::tests::{
		PgPool, add_pending_server, app, application_id, call, count, db_test, local_server_id,
		project_id, seed_deployment, stored_secrets, user_with_team,
	};
	use crate::ui::testing::{
		assert_contains, assert_not_contains, get_html, post_signals, ui_login, ui_session,
	};

	const KEY: &str =
		"-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk=\n-----END OPENSSH PRIVATE KEY-----";
	const TOKEN: &str = "ghp_supersecrettokenvalue";

	struct Fixture {
		app: Router,
		cookie: String,
		team: i64,
		project: i64,
		server: i64,
	}

	impl Fixture {
		async fn new(db: &PgPool) -> Self {
			let app = app(db.clone()).await;
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
			Self {
				app,
				cookie,
				team,
				project,
				server,
			}
		}

		fn base(&self) -> String {
			format!("/teams/{}/projects/{}/apps", self.team, self.project)
		}

		/// Signals of a valid public-repo create form, with `extra` merged in.
		fn form(&self, slug: &str, extra: Value) -> Value {
			let mut form = json!({
				"name": "Web", "slug": slug, "server_id": self.server.to_string(),
				"repo_url": "https://example.com/web.git", "git_ref": "main",
				"deploy_key": "", "git_token": "", "build_strategy": "dockerfile",
				"dockerfile_path": "Dockerfile", "container_port": "3000",
				"memory_limit_mb": "", "cpu_limit": "",
			});
			if let Value::Object(map) = extra {
				for (k, v) in map {
					form[k] = v;
				}
			}
			form
		}

		async fn create(&self, slug: &str, extra: Value) -> String {
			let (s, _, body) = post_signals(
				&self.app,
				&self.base(),
				Some(&self.cookie),
				&self.form(slug, extra),
			)
			.await;
			assert_eq!(s, StatusCode::OK);
			body
		}

		async fn get(&self, uri: &str) -> (StatusCode, String) {
			let (s, _, body) = get_html(&self.app, uri, Some(&self.cookie)).await;
			(s, body)
		}

		async fn post(&self, uri: &str, signals: &Value) -> String {
			post_signals(&self.app, uri, Some(&self.cookie), signals)
				.await
				.2
		}
	}

	/// The value of the input that follows `label` in a create response.
	fn shown_value(body: &str, label: &str) -> String {
		let after = body
			.split(label)
			.nth(1)
			.unwrap_or_else(|| panic!("no {label}: {body}"));
		let value = after.split("value=\"").nth(1).unwrap();
		value.split('"').next().unwrap().to_owned()
	}

	#[db_test]
	async fn the_create_form_offers_servers_and_shows_fields_by_signal(db: PgPool) {
		let f = Fixture::new(&db).await;
		add_pending_server(&db, f.team).await;
		let (s, body) = f.get(&format!("{}/new", f.base())).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "New application");
		assert_contains(&body, &format!("value=\"{}\"", f.server));
		assert_contains(&body, "builder (Pending)");
		// The pending server can be seen but not chosen.
		let pending = body.split("value=\"7001\"").nth(1).unwrap();
		assert!(
			pending.split('>').next().unwrap().contains("disabled"),
			"{body}"
		);
		let active = body
			.split(&format!("value=\"{}\"", f.server))
			.nth(1)
			.unwrap();
		assert!(!active.split('>').next().unwrap().contains("disabled"));
		// It is preselected: the first active server.
		assert_contains(
			&body,
			&format!("&quot;server_id&quot;:&quot;{}&quot;", f.server),
		);
		assert_contains(&body, "$build_strategy === 'dockerfile'");
		assert_contains(&body, "$repo_url.startsWith('https://')");
		assert_contains(&body, "!$repo_url.startsWith('https://')");
		assert_contains(&body, "Nixpacks");
	}

	#[db_test]
	async fn creating_shows_the_webhook_secret_once_and_never_again(db: PgPool) {
		let f = Fixture::new(&db).await;
		let body = f.create("web", json!({})).await;
		assert_contains(&body, "Application created");
		let url = shown_value(&body, "Webhook URL");
		let id = application_id(&db, "web").await;
		assert_eq!(url, format!("http://podkit.test/webhooks/deploy/{id}"));
		let secret = shown_value(&body, "Webhook secret");
		assert_eq!(secret.len(), 48, "{secret}");
		assert_contains(&body, "Copy");
		assert_contains(&body, "only this once");
		// The secret is not a signal: nothing persisted client-side.
		assert_not_contains(&body.replace(&format!("value=\"{secret}\""), ""), &secret);
		assert_eq!(stored_secrets(&db, "web").await, (false, false, true));

		let app_uri = format!("{}/{id}", f.base());
		for uri in [
			f.base() + "/new",
			app_uri.clone(),
			format!("{app_uri}/settings"),
			format!("/teams/{}/projects/{}", f.team, f.project),
			format!("/teams/{}/projects", f.team),
		] {
			let (s, page) = f.get(&uri).await;
			assert_eq!(s, StatusCode::OK, "{uri}");
			assert_not_contains(&page, &secret);
			assert_not_contains(&page, "Webhook secret");
		}
		let (_, page) = f.get(&app_uri).await;
		assert_contains(&page, &url);
		assert_contains(&page, "Web");
		assert_contains(&page, "https://example.com/web.git@main");
		assert_contains(&page, "web.127-0-0-1.sslip.io");
	}

	#[db_test]
	async fn credentials_are_stored_but_never_rendered(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.create(
			"keyed",
			json!({ "repo_url": "git@github.com:me/r.git", "deploy_key": KEY }),
		)
		.await;
		f.create("tokened", json!({ "git_token": TOKEN })).await;
		assert_eq!(stored_secrets(&db, "keyed").await, (true, false, true));
		assert_eq!(stored_secrets(&db, "tokened").await, (false, true, true));

		for slug in ["keyed", "tokened"] {
			let id = application_id(&db, slug).await;
			for uri in [
				format!("{}/{id}", f.base()),
				format!("{}/{id}/settings", f.base()),
				format!("/teams/{}/projects/{}", f.team, f.project),
			] {
				let (s, page) = f.get(&uri).await;
				assert_eq!(s, StatusCode::OK, "{uri}");
				assert_not_contains(&page, "BEGIN");
				assert_not_contains(&page, "b3BlbnNzaC1rZXk");
				assert_not_contains(&page, TOKEN);
			}
		}
		let id = application_id(&db, "keyed").await;
		let (_, page) = f.get(&format!("{}/{id}/settings", f.base())).await;
		assert_contains(&page, "A deploy key is stored.");
		let (_, page) = f.get(&format!("{}/{id}", f.base())).await;
		assert_contains(&page, "Deploy key stored");
	}

	#[db_test]
	async fn blank_credentials_on_save_keep_the_stored_ones(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.create(
			"keyed",
			json!({ "repo_url": "git@github.com:me/r.git", "deploy_key": KEY }),
		)
		.await;
		f.create("tokened", json!({ "git_token": TOKEN })).await;
		for (slug, repo) in [
			("keyed", "git@github.com:me/r.git"),
			("tokened", "https://example.com/web.git"),
		] {
			let id = application_id(&db, slug).await;
			let before = stored_secrets(&db, slug).await;
			let body = f
				.post(
					&format!("{}/{id}/settings", f.base()),
					&f.form(slug, json!({ "repo_url": repo, "name": "Renamed" })),
				)
				.await;
			assert_contains(&body, "Settings saved");
			assert_eq!(stored_secrets(&db, slug).await, before, "{slug}");
			assert_not_contains(&body, TOKEN);
			assert_not_contains(&body, "BEGIN");
		}
		let id = application_id(&db, "keyed").await;
		let (_, page) = f.get(&format!("{}/{id}", f.base())).await;
		assert_contains(&page, "Renamed");

		// A new value replaces it; the response never carries it back.
		let id = application_id(&db, "tokened").await;
		let body = f
			.post(
				&format!("{}/{id}/settings", f.base()),
				&f.form("tokened", json!({ "git_token": "ghp_replacement" })),
			)
			.await;
		assert_not_contains(&body, "ghp_replacement");
		assert_eq!(stored_secrets(&db, "tokened").await, (false, true, true));
	}

	#[db_test]
	async fn bad_input_is_an_inline_message_and_creates_nothing(db: PgPool) {
		let f = Fixture::new(&db).await;
		let cases = [
			(json!({ "repo_url": "ftp://nope" }), "invalid repo_url"),
			(
				json!({ "git_token": TOKEN, "repo_url": "git@github.com:me/r.git" }),
				"git_token requires an https://",
			),
			(
				json!({ "deploy_key": KEY }),
				"deploy_key requires an ssh://",
			),
			(
				json!({ "container_port": "abc" }),
				"Container port must be a number.",
			),
			(
				json!({ "container_port": "" }),
				"Container port is required.",
			),
			(json!({ "container_port": "70000" }), "between 1 and 65535"),
			(
				json!({ "memory_limit_mb": "lots" }),
				"Memory limit must be a number.",
			),
			(
				json!({ "cpu_limit": "0" }),
				"CPU limit must be greater than 0.",
			),
			(
				json!({ "dockerfile_path": "../etc/passwd" }),
				"invalid dockerfile_path",
			),
			(json!({ "name": "" }), "Name must be"),
			(json!({ "slug": "Not A Slug" }), "Slug must be"),
			(json!({ "server_id": "" }), "Pick a server."),
		];
		for (extra, message) in cases {
			let body = f.create("web", extra.clone()).await;
			assert_contains(&body, message);
			assert_contains(&body, "_error");
			assert_not_contains(&body, "Application created");
			assert_eq!(count(&db, "applications").await, 0, "{extra}");
		}

		f.create("web", json!({})).await;
		let body = f.create("web", json!({})).await;
		assert_contains(&body, "already used by another application");
		assert_eq!(count(&db, "applications").await, 1);

		// The same rules on save, and a slug conflict shows inline too.
		f.create("other", json!({})).await;
		let id = application_id(&db, "other").await;
		let uri = format!("{}/{id}/settings", f.base());
		let body = f.post(&uri, &f.form("web", json!({}))).await;
		assert_contains(&body, "already used by another application");
		let body = f
			.post(&uri, &f.form("other", json!({ "repo_url": "nope" })))
			.await;
		assert_contains(&body, "invalid repo_url");
	}

	#[db_test]
	async fn a_non_active_server_cannot_be_chosen_by_a_forged_post(db: PgPool) {
		let f = Fixture::new(&db).await;
		// Another team's server is not found at all.
		let (_, other_team) = user_with_team(&f.app, "bob").await;
		let theirs = local_server_id(&db, other_team).await;
		let body = f
			.create("web", json!({ "server_id": theirs.to_string() }))
			.await;
		assert_contains(&body, "That no longer exists.");
		assert_eq!(count(&db, "applications").await, 0);
	}

	#[db_test]
	async fn settings_are_prefilled_and_saving_toasts(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.create(
			"web",
			json!({ "memory_limit_mb": "256", "cpu_limit": "0.5", "build_strategy": "nixpacks" }),
		)
		.await;
		let id = application_id(&db, "web").await;
		let uri = format!("{}/{id}/settings", f.base());
		let (s, page) = f.get(&uri).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&page, "&quot;name&quot;:&quot;Web&quot;");
		assert_contains(&page, "&quot;memory_limit_mb&quot;:&quot;256&quot;");
		assert_contains(&page, "&quot;cpu_limit&quot;:&quot;0.5&quot;");
		assert_contains(&page, "&quot;build_strategy&quot;:&quot;nixpacks&quot;");
		assert_contains(&page, "Danger zone");
		assert_not_contains(&page, "&quot;server_id&quot;");

		let body = f
			.post(
				&uri,
				&f.form(
					"web2",
					json!({ "name": "Web 2", "container_port": "8080", "memory_limit_mb": "" }),
				),
			)
			.await;
		assert_contains(&body, "Settings saved");
		assert_contains(&body, "app-header");
		assert_contains(&body, "web2.127-0-0-1.sslip.io");
		let (_, page) = f.get(&format!("{}/{id}", f.base())).await;
		assert_contains(&page, "Web 2");
		assert_contains(&page, "8080");
		assert_contains(&page, "No limit");
	}

	#[db_test]
	async fn deleting_needs_the_slug_typed_exactly(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.create("doomed", json!({})).await;
		let id = application_id(&db, "doomed").await;
		let uri = format!("{}/{id}/delete", f.base());

		let body = f.post(&uri, &json!({ "confirm": "nope" })).await;
		assert_contains(&body, "doesn't match");
		assert_eq!(count(&db, "applications").await, 1);

		let body = f.post(&uri, &json!({ "confirm": "doomed" })).await;
		assert_contains(
			&body,
			&format!(
				"window.location.assign('/teams/{}/projects/{}')",
				f.team, f.project
			),
		);
		assert_eq!(count(&db, "applications").await, 0);
		let (s, _) = f.get(&format!("{}/{id}", f.base())).await;
		assert_eq!(s, StatusCode::NOT_FOUND);
	}

	#[db_test]
	async fn the_project_overview_links_apps_with_url_and_latest_status(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.create("web", json!({})).await;
		f.create("idle", json!({ "name": "Idle" })).await;
		let id = application_id(&db, "web").await;
		seed_deployment(&db, id, "running").await;

		let (s, page) = f
			.get(&format!("/teams/{}/projects/{}", f.team, f.project))
			.await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&page, &format!("href=\"{}/{id}\"", f.base()));
		assert_contains(&page, "web.127-0-0-1.sslip.io");
		assert_contains(&page, "Running");
		assert_contains(&page, "Not deployed");
		assert_contains(&page, "New application");
		assert_contains(&page, &format!("{}/new", f.base()));
	}

	#[db_test]
	async fn names_are_escaped(db: PgPool) {
		let f = Fixture::new(&db).await;
		let name = "<script>alert(1)</script>";
		f.create("xss", json!({ "name": name, "git_ref": "<b>x</b>" }))
			.await;
		let id = application_id(&db, "xss").await;
		for uri in [
			format!("{}/{id}", f.base()),
			format!("{}/{id}/settings", f.base()),
			format!("/teams/{}/projects/{}", f.team, f.project),
		] {
			let (s, page) = f.get(&uri).await;
			assert_eq!(s, StatusCode::OK, "{uri}");
			assert_not_contains(&page, name);
			assert_not_contains(&page, "<b>x</b>");
			assert_contains(&page, "&lt;script&gt;alert(1)&lt;/script&gt;");
		}
	}

	#[db_test]
	async fn another_teams_application_is_the_404_page_and_cannot_be_changed(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.create(
			"secret-app",
			json!({ "name": "Secret app", "git_token": TOKEN }),
		)
		.await;
		let id = application_id(&db, "secret-app").await;
		let (bob_cookie, bob_team) = ui_session(&f.app, "bob").await;

		for uri in [
			// Alice's team, Alice's project.
			format!("/teams/{}/projects/{}/apps/{id}", f.team, f.project),
			format!(
				"/teams/{}/projects/{}/apps/{id}/settings",
				f.team, f.project
			),
			format!("/teams/{}/projects/{}/apps/new", f.team, f.project),
			// Bob's own team with Alice's ids.
			format!("/teams/{bob_team}/projects/{}/apps/{id}", f.project),
			format!("/teams/{bob_team}/projects/{}/apps/new", f.project),
			format!("/teams/{bob_team}/projects/{}", f.project),
		] {
			let (s, _, body) = get_html(&f.app, &uri, Some(&bob_cookie)).await;
			assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
			assert_contains(&body, "Not found");
			assert_not_contains(&body, "Secret app");
			assert_not_contains(&body, "secret-app");
		}

		let before = stored_secrets(&db, "secret-app").await;
		for (uri, signals) in [
			(
				format!("{}/{id}/settings", f.base()),
				f.form("secret-app", json!({ "name": "Hacked" })),
			),
			(
				format!("{}/{id}/delete", f.base()),
				json!({ "confirm": "secret-app" }),
			),
			(f.base(), f.form("planted", json!({}))),
			(
				format!("/teams/{bob_team}/projects/{}/apps/{id}/delete", f.project),
				json!({ "confirm": "secret-app" }),
			),
			(
				format!(
					"/teams/{bob_team}/projects/{}/apps/{id}/settings",
					f.project
				),
				f.form("secret-app", json!({ "name": "Hacked" })),
			),
		] {
			let (_, _, body) = post_signals(&f.app, &uri, Some(&bob_cookie), &signals).await;
			assert_contains(&body, "That no longer exists.");
			assert_not_contains(&body, "Application created");
		}
		assert_eq!(count(&db, "applications").await, 1);
		assert_eq!(stored_secrets(&db, "secret-app").await, before);
		let (_, page) = f.get(&format!("{}/{id}", f.base())).await;
		assert_contains(&page, "Secret app");
		assert_not_contains(&page, "Hacked");
	}

	/// Adds `name` to `team` with the built-in read-only Member role (id 2).
	async fn join_as_member(app: &Router, owner_token: &str, team: i64, name: &str) {
		let (s, _) = call(
			app,
			"POST",
			&format!("/api/v1/teams/{team}/members"),
			Some(owner_token),
			Some(json!({ "email": format!("{name}@example.com"), "role_id": 2 })),
		)
		.await;
		assert!(s.is_success(), "add member: {s}");
	}

	#[db_test]
	async fn a_read_only_member_sees_no_actions_and_forged_posts_fail(db: PgPool) {
		let f = Fixture::new(&db).await;
		f.create("web", json!({ "name": "Shared" })).await;
		let id = application_id(&db, "web").await;
		let owner_token = {
			let creds =
				json!({ "email": "alice@example.com", "password": "correct horse battery" });
			let (_, login) = call(&f.app, "POST", "/api/v1/auth/login", None, Some(creds)).await;
			login["token"].as_str().unwrap().to_owned()
		};
		user_with_team(&f.app, "carol").await;
		join_as_member(&f.app, &owner_token, f.team, "carol").await;
		let carol = ui_login(&f.app, "carol").await;

		let (s, _, page) = get_html(&f.app, &format!("{}/{id}", f.base()), Some(&carol)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&page, "Shared");
		assert_not_contains(&page, "Settings");
		let (s, _, page) = get_html(
			&f.app,
			&format!("/teams/{}/projects/{}", f.team, f.project),
			Some(&carol),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		assert_not_contains(&page, "New application");

		for uri in [
			format!("{}/{id}/settings", f.base()),
			format!("{}/new", f.base()),
		] {
			let (s, _, page) = get_html(&f.app, &uri, Some(&carol)).await;
			assert_eq!(s, StatusCode::FORBIDDEN, "{uri}");
			assert_contains(&page, "permission");
		}

		for (uri, signals) in [
			(f.base(), f.form("forged", json!({}))),
			(
				format!("{}/{id}/settings", f.base()),
				f.form("web", json!({ "name": "Forged" })),
			),
			(
				format!("{}/{id}/delete", f.base()),
				json!({ "confirm": "web" }),
			),
		] {
			let (_, _, body) = post_signals(&f.app, &uri, Some(&carol), &signals).await;
			assert_contains(&body, "You don't have permission to do that.");
		}
		assert_eq!(count(&db, "applications").await, 1);
		let (_, page) = f.get(&format!("{}/{id}", f.base())).await;
		assert_not_contains(&page, "Forged");

		// The owner has the tab and the forms.
		let (_, page) = f.get(&format!("{}/{id}", f.base())).await;
		assert_contains(&page, "Settings");
		let (_, page) = f.get(&format!("{}/{id}/settings", f.base())).await;
		assert_contains(&page, "Delete application");
	}

	#[db_test]
	async fn the_json_api_still_creates_updates_and_deletes(db: PgPool) {
		let app = app(db.clone()).await;
		let (token, team) = user_with_team(&app, "alice").await;
		let (_, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "P", "slug": "p" })),
		)
		.await;
		let project = project["id"].as_i64().unwrap();
		let server = local_server_id(&db, team).await;
		let base = format!("/api/v1/teams/{team}/projects/{project}/applications");
		let body = json!({
			"name": "A", "slug": "a", "server_id": server,
			"repo_url": "https://example.com/a.git", "container_port": 80,
		});
		let (s, created) = call(&app, "POST", &base, Some(&token), Some(body)).await;
		assert_eq!(s, StatusCode::OK, "{created}");
		assert!(
			created["webhook_secret"]
				.as_str()
				.is_some_and(|s| s.len() == 48)
		);
		let id = created["id"].as_i64().unwrap();

		let (_, list) = call(&app, "GET", &base, Some(&token), None).await;
		assert_eq!(list[0]["slug"], "a");
		assert!(list[0].get("webhook_secret").is_none());

		let (s, updated) = call(
			&app,
			"PATCH",
			&format!("{base}/{id}"),
			Some(&token),
			Some(json!({
				"name": "B", "slug": "a", "repo_url": "https://example.com/a.git",
				"container_port": 81,
			})),
		)
		.await;
		assert_eq!(s, StatusCode::OK, "{updated}");
		assert_eq!(updated["name"], "B");

		let (s, _) = call(&app, "DELETE", &format!("{base}/{id}"), Some(&token), None).await;
		assert_eq!(s, StatusCode::OK);
		assert_eq!(count(&db, "applications").await, 0);
	}
}
