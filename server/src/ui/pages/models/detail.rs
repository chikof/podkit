//! A model's page, an Operate surface: a header (logo, name, status, Start / Stop /
//! Restart) over tabs, each its own `?tab=` of this one route: General, Playground, Links,
//! Usage, Logs, Advanced. The playground lives in [`super::playground`].
//!
//! Removing is on the Advanced tab behind a confirmation dialog. What a person may not do
//! is not rendered, and the service refuses a forged request anyway.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::{human_bytes, model_url, models_url, playground};
use crate::auth::permission::{Action, Resource};
use crate::error::ServerError;
use crate::ids::{ApplicationId, ModelId, ProjectId, TeamId};
use crate::links::service::{self as links_service, ModelLinks};
use crate::model_files::ModelFile;
use crate::models::service::{self as models_service, LogSource};
use crate::models::{Model, ModelStatus, engines};
use crate::projects;
use crate::projects::overview::model_mark;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON_DANGER, BUTTON_OUTLINE, Dismiss, LABEL, LogoSize, SELECT, Size, TabItem, Tone, Variant,
	badge, button_class, card, card_content, card_section, dialog, dialog_open_button, empty_state,
	icon, logo, table, tabs,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::live::{container_log, job_log, log_view};
use crate::ui::sse::js_string;
use crate::ui::status::{Status, poll, status_badge};
use crate::ui::{error, sse};

type Ids = Path<(TeamId, ProjectId, ModelId)>;

pub fn router() -> Router<AppState> {
	const M: &str = "/teams/{team_id}/projects/{project_id}/models/{model_id}";
	Router::new()
		.route(M, get(page))
		.route(&format!("{M}/status"), get(status))
		.route(&format!("{M}/logs"), get(logs))
		.route(&format!("{M}/start"), post(start))
		.route(&format!("{M}/stop"), post(stop))
		.route(&format!("{M}/restart"), post(restart))
		.route(&format!("{M}/remove"), post(remove))
		.route(&format!("{M}/link"), post(link))
		.route(&format!("{M}/link-preview"), get(link_preview))
		.route(&format!("{M}/unlink"), post(unlink))
}

fn url(ctx: &TeamCtx, project: ProjectId, model: ModelId, rest: &str) -> String {
	format!("{}{rest}", model_url(ctx.team.id, project, model))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
	General,
	Playground,
	Links,
	Usage,
	Logs,
	Advanced,
}

impl Tab {
	fn key(self) -> &'static str {
		match self {
			Self::General => "general",
			Self::Playground => "playground",
			Self::Links => "links",
			Self::Usage => "usage",
			Self::Logs => "logs",
			Self::Advanced => "advanced",
		}
	}

	fn label(self) -> &'static str {
		match self {
			Self::General => "General",
			Self::Playground => "Playground",
			Self::Links => "Links",
			Self::Usage => "Usage",
			Self::Logs => "Logs",
			Self::Advanced => "Advanced",
		}
	}

	fn parse(s: Option<&str>) -> Self {
		match s {
			Some("playground") => Self::Playground,
			Some("links") => Self::Links,
			Some("usage") => Self::Usage,
			Some("logs") => Self::Logs,
			Some("advanced") => Self::Advanced,
			_ => Self::General,
		}
	}

	fn href(self, ctx: &TeamCtx, model: &Model) -> String {
		match self {
			Self::General => url(ctx, model.project_id, model.id, ""),
			other => url(
				ctx,
				model.project_id,
				model.id,
				&format!("?tab={}", other.key()),
			),
		}
	}
}

/// The tabs this caller may see: the playground is for those who may use the model.
fn visible_tabs(ctx: &TeamCtx) -> Vec<Tab> {
	let mut tabs = vec![Tab::General];
	if ctx.perms.can(Action::Update, Resource::Model) {
		tabs.push(Tab::Playground);
	}
	tabs.extend([Tab::Links, Tab::Usage, Tab::Logs, Tab::Advanced]);
	tabs
}

// --- the header ---------------------------------------------------------------------------

/// Logo, name, status and the buttons. The whole block is replaced by every poll and every
/// action, so it ends the polling itself once the status settles.
fn header(ctx: &TeamCtx, model: &Model, files: Option<&ModelFile>, project_name: &str) -> Markup {
	let post = |action: &str| {
		format!(
			"@post({})",
			js_string(&url(ctx, model.project_id, model.id, &format!("/{action}")))
		)
	};
	let can_update = ctx.perms.can(Action::Update, Resource::Model);
	let removing = model.status == ModelStatus::Removing;
	let poll_url = url(
		ctx,
		model.project_id,
		model.id,
		&format!("/status?logs={}", u8::from(model.container_id.is_some())),
	);
	html! {
		div id="model-status" class="flex flex-col gap-4" {
			div class="flex flex-wrap items-start justify-between gap-x-6 gap-y-4" {
				div class="flex min-w-0 items-center gap-4" {
					(logo(model_mark(model.engine), LogoSize::Lg))
					div class="flex min-w-0 flex-col gap-1.5" {
						div class="flex flex-wrap items-center gap-x-3 gap-y-1" {
							h1 class="min-w-0 truncate text-2xl font-semibold tracking-tight" { (model.name) }
							(badge(Tone::Neutral, "Model"))
							(status_badge(model.status))
						}
						p class="text-sm text-muted-foreground" {
							(project_name)
							@if let Some(f) = files {
								" · " (f.repo) " · " (human_bytes(u64::try_from(f.total_bytes).unwrap_or(0)))
							}
						}
						@if model.status == ModelStatus::Starting {
							p class="text-sm text-muted-foreground" { "Loading the weights. Big models take a few minutes." }
						}
					}
				}
				@if can_update && !removing {
					div class="flex flex-wrap gap-2" {
						@if matches!(model.status, ModelStatus::Stopped | ModelStatus::Failed) {
							button type="button" class=(button_class(Variant::Default, Size::Md)) "data-on:click"=(post("start")) { (icon("play")) "Start" }
						}
						@if matches!(model.status, ModelStatus::Running | ModelStatus::Starting) {
							button type="button" class=(button_class(Variant::Outline, Size::Md)) "data-on:click"=(post("stop")) { (icon("square")) "Stop" }
						}
						button type="button" class=(button_class(Variant::Outline, Size::Md)) "data-on:click"=(post("restart")) { (icon("refresh-cw")) "Restart" }
					}
				}
			}
			@if let Some(err) = &model.error {
				pre class="max-h-48 overflow-auto rounded-lg border border-border bg-card p-3 font-mono text-xs whitespace-pre-wrap text-destructive" role="alert" { (err) }
			}
			(poll(&poll_url, model.status.in_flight()))
		}
	}
}

// --- the tabs -----------------------------------------------------------------------------

/// A label and its value, one per row; the value may be long (a URL, an image).
fn facts(rows: &[(&str, Markup)]) -> Markup {
	html! {
		dl class="flex flex-col" {
			@for (label, value) in rows {
				div class="flex flex-wrap justify-between gap-x-6 gap-y-1 border-t border-border py-3 text-sm first:border-t-0 first:pt-0 last:pb-0" {
					dt class="text-muted-foreground" { (label) }
					dd class="min-w-0 break-all sm:text-right" { (value) }
				}
			}
		}
	}
}

fn general_tab(model: &Model, files: Option<&ModelFile>) -> Markup {
	let spec = engines::spec(model.engine);
	card(&html! {
		(card_section("How it runs", Some("What this model is served with. Other containers on the server reach it at the internal URL."), None))
		(card_content(&facts(&[
			("Engine", html! { (spec.label) }),
			("Recipe", html! {
				@if let Some(f) = files {
					code { (f.repo) } " @ " code { (f.revision.chars().take(8).collect::<String>()) }
					@if !model.model_path.is_empty() { " · " code { (model.model_path) } }
				} @else { "The weights were removed." }
			}),
			("Served as", html! { code { (model.served_name) } }),
			("Internal URL", html! { code { (model.internal_url()) } }),
			("Context", html! { (model.context_size) " tokens" }),
			("Runs on", html! { @if model.gpu { "GPU" } @else { "CPU" } }),
		])))
	})
}

fn advanced_tab(ctx: &TeamCtx, model: &Model) -> Markup {
	let can_delete = ctx.perms.can(Action::Delete, Resource::Model);
	let removing = model.status == ModelStatus::Removing;
	let remove = format!(
		"@post({})",
		js_string(&url(ctx, model.project_id, model.id, "/remove"))
	);
	html! {
		div class="flex flex-col gap-6" {
			(card(&html! {
				(card_section("Engine settings", Some("Read only. The model's container is built from these when it starts."), None))
				(card_content(&facts(&[
					("Image", html! { code { (model.image) } }),
					("Model file", html! { code { @if model.model_path.is_empty() { "the whole download" } @else { (model.model_path) } } }),
					("Memory limit", html! { @if let Some(mb) = model.memory_limit_mb { (mb) " MB" } @else { "None" } }),
					("Extra arguments", html! { @if model.extra_args.0.is_empty() { "None" } @else { code { (model.extra_args.0.join(" ")) } } }),
				])))
			}))
			@if can_delete && !removing {
				(card(&html! {
					(card_section("Remove this model", Some("Stops it and removes its container. The downloaded files stay on the server."), None))
					(card_content(&html! {
						(dialog_open_button("remove-dialog", BUTTON_DANGER, &html! { (icon("trash-2")) "Remove model" }))
					}))
				}))
				(dialog(
					"remove-dialog",
					&format!("Remove {}?", model.name),
					Some("Applications linked to it lose it on their next deploy. The downloaded files stay."),
					Dismiss::Esc,
					&html! {},
					&html! {
						form method="dialog" class="flex flex-col-reverse gap-2 sm:flex-row" {
							button class=(BUTTON_OUTLINE) autofocus { "Keep it" }
							button class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"=(remove) { "Remove" }
						}
					},
				))
			}
		}
	}
}

fn logs_tab(ctx: &TeamCtx, model: &Model) -> Markup {
	let logs_url = url(ctx, model.project_id, model.id, "/logs");
	html! {
		div id="model-log-card" {
			(card(&html! {
				(card_section("Log", Some(if model.container_id.is_some() { "The engine's own output." } else { "The deploy, until the engine has a container." }), None))
				(card_content(&log_view("model-log", &logs_url)))
			}))
		}
	}
}

fn preview_table(vars: &[(String, String)]) -> Markup {
	html! {
		div id="link-preview" class="flex flex-col gap-2 text-sm" {
			@if !vars.is_empty() {
				p class="text-muted-foreground" { "The application gets these on its next deploy. The key is masked." }
				dl class="flex flex-col gap-1 rounded-lg border border-border bg-muted p-3 font-mono text-xs" {
					@for (name, value) in vars {
						div class="flex flex-wrap gap-x-2" {
							dt class="font-semibold" { (name) }
							dd class="break-all text-muted-foreground" { (value) }
						}
					}
				}
			}
		}
	}
}

fn links_card(ctx: &TeamCtx, model: &Model, links: &ModelLinks) -> Markup {
	let can_link = ctx.perms.can(Action::Update, Resource::Application);
	let unlink = |app: ApplicationId| {
		format!(
			"@post({})",
			js_string(&url(
				ctx,
				model.project_id,
				model.id,
				&format!("/unlink?app={app}")
			))
		)
	};
	let preview = format!(
		"$linkApp && @get({})",
		js_string(&url(ctx, model.project_id, model.id, "/link-preview"))
	);
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&url(ctx, model.project_id, model.id, "/link"))
	);
	html! {
		div id="model-links" {
			(card(&html! {
				(card_section("Linked applications", Some("A linked application reaches this model through the gateway with its own key. Changes apply on the application's next deploy."), None))
				(card_content(&html! {
					div class="flex flex-col gap-5" {
						@if links.linked.is_empty() {
							p class="text-sm text-muted-foreground" { "No application uses this model yet." }
						} @else {
							ul class="flex flex-col" {
								@for app in &links.linked {
									li class="flex flex-wrap items-center justify-between gap-3 border-t border-border py-3 text-sm first:border-t-0 first:pt-0" {
										span class="flex items-center gap-2 font-medium" {
											(app.name)
											@if app.is_default { (badge(Tone::Neutral, "default")) }
										}
										@if can_link {
											button type="button" class=(button_class(Variant::Outline, Size::Sm)) "data-on:click"=(unlink(app.application_id)) { "Unlink" }
										}
									}
								}
							}
						}
						@if can_link && !links.linkable.is_empty() {
							form class="flex flex-col gap-3 border-t border-border pt-5" "data-on:submit"=(post) {
								label class=(LABEL) {
									"Link an application"
									select class=(SELECT) required "data-bind"="linkApp" "data-on:change"=(preview) {
										option value="" { "Choose…" }
										@for (id, name) in &links.linkable { option value=(id.0) { (name) } }
									}
								}
								label class="flex items-center gap-2 text-sm" {
									input type="checkbox" "data-bind"="linkDefault";
									"Make it the model PODKIT_MODEL names"
								}
								(preview_table(&[]))
								(form_error())
								div {
									button type="submit" class=(button_class(Variant::Default, Size::Md)) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Link" }
								}
							}
						}
					}
				}))
			}))
		}
	}
}

fn usage_tab(rows: &[crate::links::KeyUsage]) -> Markup {
	if rows.is_empty() {
		return empty_state(
			"activity",
			"Nothing has used this model in the last 7 days.",
			Some("Requests through the gateway show up here, per key."),
			None,
		);
	}
	let n = |v: i64| v.to_string();
	let table_rows: Vec<Vec<Markup>> = rows
		.iter()
		.map(|r| {
			vec![
				html! { (r.key_label) },
				html! { (n(r.requests_24h)) },
				html! { (n(r.input_24h)) " / " (n(r.output_24h)) },
				html! { (n(r.requests_7d)) },
				html! { (n(r.input_7d)) " / " (n(r.output_7d)) },
			]
		})
		.collect();
	card(&html! {
		(card_section("Usage", Some("Requests and tokens that went through the gateway, per key."), None))
		(card_content(&table(
			"model-usage",
			"Usage by key",
			&["Key", "Requests 24 h", "In / out 24 h", "Requests 7 d", "In / out 7 d"],
			&table_rows,
		)))
	})
}

// --- the page -----------------------------------------------------------------------------

#[derive(Deserialize)]
struct PageQuery {
	tab: Option<String>,
}

async fn page(
	State(s): State<AppState>,
	mut ctx: TeamCtx,
	Path((_, project, model_id)): Ids,
	Query(q): Query<PageQuery>,
) -> Result<Response, Response> {
	let model = models_service::get(&s, ctx.user.id, ctx.team.id, project, model_id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let fail = |e: ServerError| error::page(&ctx, &e);
	let project_row = projects::service::get(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	// The header's breadcrumb names the environment the model lives in.
	if let Some(scope) = ctx.scope.as_mut()
		&& let Some(env) = scope
			.environments
			.iter()
			.find(|e| e.id == model.environment_id)
	{
		scope.environment = env.clone();
	}
	let files =
		crate::model_files::service::get(&s, ctx.user.id, ctx.team.id, model.model_files_id)
			.await
			.ok();

	let tab = Tab::parse(q.tab.as_deref());
	let allowed = visible_tabs(&ctx);
	let tab = if allowed.contains(&tab) {
		tab
	} else {
		Tab::General
	};
	let content = match tab {
		Tab::General => general_tab(&model, files.as_ref()),
		Tab::Playground => playground::chat_panel(&ctx, &model),
		Tab::Links => {
			let links = if ctx.perms.can(Action::Read, Resource::Application) {
				links_service::for_model(&s, ctx.user.id, ctx.team.id, project, model_id)
					.await
					.map_err(|e| error::page(&ctx, &e))?
			} else {
				ModelLinks {
					linked: Vec::new(),
					linkable: Vec::new(),
				}
			};
			links_card(&ctx, &model, &links)
		}
		Tab::Usage => {
			let usage = links_service::usage(&s, ctx.user.id, ctx.team.id, project, model_id)
				.await
				.map_err(|e| error::page(&ctx, &e))?;
			usage_tab(&usage)
		}
		Tab::Logs => logs_tab(&ctx, &model),
		Tab::Advanced => advanced_tab(&ctx, &model),
	};
	let hrefs: Vec<(Tab, String)> = allowed.iter().map(|t| (*t, t.href(&ctx, &model))).collect();
	let items: Vec<TabItem<'_>> = hrefs
		.iter()
		.map(|(t, href)| TabItem {
			label: t.label(),
			href,
			current: *t == tab,
		})
		.collect();

	let body = html! {
		div class="ui-wide flex flex-col gap-6" data-signals="{linkApp: '', linkDefault: false, _error: ''}" {
			(header(&ctx, &model, files.as_ref(), &project_row.project.name))
			(tabs("Model sections", &items))
			(content)
		}
	};
	let name = model.name.clone();
	let team = ctx.team.id;
	let project_crumb = project_row.project.name.clone();
	Ok(app_page(
		&ctx,
		&name,
		&[
			("Projects", Some(format!("/teams/{team}/projects"))),
			(
				project_crumb.as_str(),
				Some(format!("/teams/{team}/projects/{project}")),
			),
			(name.as_str(), None),
		],
		&body,
	)
	.into_response())
}

#[derive(Deserialize)]
struct StatusQuery {
	/// Whether the page already shows the engine's own log.
	#[serde(default)]
	logs: u8,
}

/// The poll target: the header, plus the engine log once it exists.
async fn status(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, model_id)): Ids,
	Query(q): Query<StatusQuery>,
) -> Response {
	match models_service::get(&s, ctx.user.id, ctx.team.id, project, model_id).await {
		Ok(model) => {
			let (files, project_name) = header_inputs(&s, &ctx, &model).await;
			let events = sse::patch(&header(&ctx, &model, files.as_ref(), &project_name));
			if q.logs == 0 && model.container_id.is_some() {
				events
					.and(sse::patch(&logs_tab(&ctx, &model)))
					.into_response()
			} else {
				events.into_response()
			}
		}
		// The remove job deletes the row; there is nothing left to show.
		Err(ServerError::ModelNotFound) => sse::redirect(&models_url(ctx.team.id)).into_response(),
		Err(e) => error::action(&e),
	}
}

/// What the header shows besides the model: its files and its project's name. Best effort,
/// since the header is about the model and these are only the line under its name.
async fn header_inputs(s: &AppState, ctx: &TeamCtx, model: &Model) -> (Option<ModelFile>, String) {
	let files = crate::model_files::service::get(s, ctx.user.id, ctx.team.id, model.model_files_id)
		.await
		.ok();
	let name = projects::service::get(&s.db, ctx.user.id, ctx.team.id, model.project_id)
		.await
		.map(|p| p.project.name)
		.unwrap_or_default();
	(files, name)
}

async fn logs(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, model_id)): Ids,
) -> Response {
	match models_service::log_source(&s, ctx.user.id, ctx.team.id, project, model_id, 200).await {
		Ok(LogSource::Container(events)) => container_log(events, "model-log", s.shutdown.clone()),
		Ok(LogSource::Job(job)) => job_log(&s, job, "model-log"),
		Err(e) => error::action(&e),
	}
}

/// Runs a service call that returns the changed model, then shows it.
async fn act(
	s: &AppState,
	ctx: &TeamCtx,
	result: Result<Model, ServerError>,
	done: &str,
) -> Response {
	match result {
		Ok(model) => {
			let (files, project_name) = header_inputs(s, ctx, &model).await;
			sse::patch(&header(ctx, &model, files.as_ref(), &project_name))
				.and(sse::toast(done))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn start(State(s): State<AppState>, ctx: TeamCtx, Path((_, p, m)): Ids) -> Response {
	let r = models_service::start(&s, ctx.user.id, ctx.team.id, p, m).await;
	act(&s, &ctx, r, "Starting").await
}

async fn stop(State(s): State<AppState>, ctx: TeamCtx, Path((_, p, m)): Ids) -> Response {
	let r = models_service::stop(&s, ctx.user.id, ctx.team.id, p, m).await;
	act(&s, &ctx, r, "Stopping").await
}

async fn restart(State(s): State<AppState>, ctx: TeamCtx, Path((_, p, m)): Ids) -> Response {
	let r = models_service::restart(&s, ctx.user.id, ctx.team.id, p, m).await;
	act(&s, &ctx, r, "Restarting").await
}

async fn remove(State(s): State<AppState>, ctx: TeamCtx, Path((_, p, m)): Ids) -> Response {
	let r = models_service::remove(&s, ctx.user.id, ctx.team.id, p, m).await;
	act(&s, &ctx, r, "Removing").await
}

#[derive(Deserialize)]
struct LinkSignals {
	#[serde(rename = "linkApp")]
	app: String,
	#[serde(rename = "linkDefault", default)]
	default: bool,
}

fn parse_app(s: &str) -> Result<ApplicationId, ServerError> {
	s.parse::<i64>()
		.map(ApplicationId)
		.map_err(|_| ServerError::Validation("Choose an application.".to_owned()))
}

async fn link_preview(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, model_id)): Ids,
	ReadSignals(sig): ReadSignals<LinkSignals>,
) -> Response {
	let result = async {
		let app = parse_app(&sig.app)?;
		links_service::preview(&s, ctx.user.id, ctx.team.id, project, app, model_id).await
	}
	.await;
	match result {
		Ok(vars) => sse::patch(&preview_table(&vars)).into_response(),
		Err(e) => error::action(&e),
	}
}

/// Re-renders the links card after a change.
async fn links_response(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	model_id: ModelId,
	toast: &str,
) -> Response {
	let result = async {
		let model = models_service::get(s, ctx.user.id, ctx.team.id, project, model_id).await?;
		let links =
			links_service::for_model(s, ctx.user.id, ctx.team.id, project, model_id).await?;
		Ok::<_, ServerError>((model, links))
	}
	.await;
	match result {
		Ok((model, links)) => sse::patch(&links_card(ctx, &model, &links))
			.and(sse::signals(
				&json!({ "linkApp": "", "linkDefault": false, "_error": "" }),
			))
			.and(sse::toast(toast))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

async fn link(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, model_id)): Ids,
	ReadSignals(sig): ReadSignals<LinkSignals>,
) -> Response {
	let result = async {
		let app = parse_app(&sig.app)?;
		links_service::link(
			&s,
			ctx.user.id,
			ctx.team.id,
			project,
			app,
			model_id,
			sig.default,
		)
		.await
	}
	.await;
	match result {
		Ok(_) => {
			links_response(
				&s,
				&ctx,
				project,
				model_id,
				"Linked. Redeploy the application to give it the new variables.",
			)
			.await
		}
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize)]
struct AppQuery {
	app: String,
}

async fn unlink(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, model_id)): Ids,
	Query(q): Query<AppQuery>,
) -> Response {
	let result = async {
		let app = parse_app(&q.app)?;
		links_service::unlink(&s, ctx.user.id, ctx.team.id, project, app, model_id).await
	}
	.await;
	match result {
		Ok(()) => {
			links_response(
				&s,
				&ctx,
				project,
				model_id,
				"Unlinked. The running application keeps its variables until its next deploy.",
			)
			.await
		}
		Err(e) => error::action(&e),
	}
}
