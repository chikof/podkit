//! A model's page: status, how it runs, start/stop/restart/remove, logs, the
//! applications it is linked to, and its usage. The playground lives in
//! [`super::playground`].

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::{crumbs, human_bytes, model_url, models_url, playground};
use crate::auth::permission::{Action, Resource};
use crate::error::ServerError;
use crate::ids::{ApplicationId, ModelId, ProjectId, TeamId};
use crate::links::service::{self as links_service, ModelLinks};
use crate::models::service::{self as models_service, LogSource};
use crate::models::{Model, ModelStatus, engines};
use crate::projects;
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, BUTTON_DANGER, BUTTON_OUTLINE, INPUT, LABEL, Tone, card, card_content, card_header,
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

/// The status, the engine's complaint when it failed, and the buttons. The
/// whole region is replaced by every poll, so it ends the polling itself.
fn status_region(ctx: &TeamCtx, model: &Model) -> Markup {
	let post = |action: &str| {
		format!(
			"@post({})",
			js_string(&url(ctx, model.project_id, model.id, &format!("/{action}")))
		)
	};
	let can_update = ctx.perms.can(Action::Update, Resource::Model);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Model);
	let removing = model.status == ModelStatus::Removing;
	let remove = format!(
		"confirm('Remove this model? The downloaded files stay.') && {}",
		post("remove")
	);
	let poll_url = url(
		ctx,
		model.project_id,
		model.id,
		&format!("/status?logs={}", u8::from(model.container_id.is_some())),
	);
	html! {
		div id="model-status" class="flex flex-col gap-4" {
			div class="flex flex-wrap items-center gap-3" {
				(status_badge(model.status))
				@if model.status == ModelStatus::Starting {
					span class="text-sm text-muted-foreground" { "Loading the weights. Big models take a few minutes." }
				}
			}
			@if let Some(err) = &model.error {
				pre class="max-h-48 overflow-auto rounded-lg border border-border bg-card p-3 font-mono text-xs whitespace-pre-wrap text-destructive" role="alert" { (err) }
			}
			div class="flex flex-wrap gap-3" {
				@if can_update && matches!(model.status, ModelStatus::Stopped | ModelStatus::Failed) {
					button type="button" class=(BUTTON) "data-on:click"=(post("start")) { "Start" }
				}
				@if can_update && matches!(model.status, ModelStatus::Running | ModelStatus::Starting) {
					button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(post("stop")) { "Stop" }
				}
				@if can_update && !removing {
					button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(post("restart")) { "Restart" }
				}
				@if can_delete && !removing {
					button type="button" class=(BUTTON_DANGER) "data-on:click"=(remove) { "Remove" }
				}
			}
			(poll(&poll_url, model.status.in_flight()))
		}
	}
}

fn facts_card(ctx: &TeamCtx, model: &Model) -> Markup {
	let spec = engines::spec(model.engine);
	let row = |label: &str, value: Markup| {
		html! {
			div class="flex flex-wrap justify-between gap-2 border-t border-border py-2 text-sm first:border-t-0" {
				span class="text-muted-foreground" { (label) }
				span class="min-w-0 text-right break-all" { (value) }
			}
		}
	};
	let _ = ctx;
	card(&html! {
		(card_header("How it runs", None))
		(card_content(&html! {
			(row("Engine", html! { (spec.label) }))
			(row("Image", html! { code { (model.image) } }))
			(row("Model file", html! { code { @if model.model_path.is_empty() { "the whole download" } @else { (model.model_path) } } }))
			(row("Context", html! { (model.context_size) " tokens" }))
			(row("Runs on", html! { @if model.gpu { "GPU" } @else { "CPU" } }))
			(row("Served as", html! { code { (model.served_name) } }))
			(row("Internal URL", html! { code { (model.internal_url()) } }))
		}))
	})
}

fn log_card(ctx: &TeamCtx, model: &Model) -> Markup {
	let logs_url = url(ctx, model.project_id, model.id, "/logs");
	html! {
		div id="model-log-card" {
			(card(&html! {
				(card_header("Log", Some(if model.container_id.is_some() { "The engine's own output." } else { "The deploy, until the engine has a container." })))
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
				dl class="flex flex-col gap-1 rounded-lg border border-border p-3 font-mono text-xs" {
					@for (name, value) in vars {
						div class="flex flex-wrap gap-2" {
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
				(card_header("Linked applications", Some("A linked application reaches this model through the gateway with its own key. Changes apply on the application's next deploy.")))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						@if links.linked.is_empty() {
							p class="text-sm text-muted-foreground" { "No application uses this model yet." }
						} @else {
							ul {
								@for app in &links.linked {
									li class="flex flex-wrap items-center justify-between gap-3 border-t border-border py-3 text-sm first:border-t-0" {
										span class="font-medium" {
											(app.name)
											@if app.is_default { " " span class=(Tone::Neutral.badge_class()) { "default" } }
										}
										@if can_link {
											button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(unlink(app.application_id)) { "Unlink" }
										}
									}
								}
							}
						}
						@if can_link && !links.linkable.is_empty() {
							form class="flex flex-col gap-3" "data-on:submit"=(post) {
								label class=(LABEL) {
									"Link an application"
									select class=(INPUT) required "data-bind:linkApp" "data-on:change"=(preview) {
										option value="" { "Choose…" }
										@for (id, name) in &links.linkable { option value=(id.0) { (name) } }
									}
								}
								label class="flex items-center gap-2 text-sm" {
									input type="checkbox" "data-bind:linkDefault";
									"Make it the model PODKIT_MODEL names"
								}
								(preview_table(&[]))
								(form_error())
								div {
									button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Link" }
								}
							}
						}
					}
				}))
			}))
		}
	}
}

fn usage_card(rows: &[crate::links::KeyUsage]) -> Markup {
	let n = |v: i64| v.to_string();
	html! {
		(card(&html! {
			(card_header("Usage", Some("Requests and tokens that went through the gateway, per key.")))
			(card_content(&html! {
				@if rows.is_empty() {
					p class="text-sm text-muted-foreground" { "Nothing has used this model in the last 7 days." }
				} @else {
					div class="overflow-x-auto" {
						table class="w-full text-left text-sm" {
							thead class="text-muted-foreground" {
								tr {
									th class="py-2 pr-4 font-medium" { "Key" }
									th class="py-2 pr-4 font-medium" { "Requests 24 h" }
									th class="py-2 pr-4 font-medium" { "In / out 24 h" }
									th class="py-2 pr-4 font-medium" { "Requests 7 d" }
									th class="py-2 font-medium" { "In / out 7 d" }
								}
							}
							tbody {
								@for r in rows {
									tr class="border-t border-border" {
										td class="py-2 pr-4 font-medium" { (r.key_label) }
										td class="py-2 pr-4" { (n(r.requests_24h)) }
										td class="py-2 pr-4" { (n(r.input_24h)) " / " (n(r.output_24h)) }
										td class="py-2 pr-4" { (n(r.requests_7d)) }
										td class="py-2" { (n(r.input_7d)) " / " (n(r.output_7d)) }
									}
								}
							}
						}
					}
				}
			}))
		}))
	}
}

async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, model_id)): Ids,
) -> Result<Response, Response> {
	let fail = |e: ServerError| error::page(&ctx, &e);
	let model = models_service::get(&s, ctx.user.id, ctx.team.id, project, model_id)
		.await
		.map_err(fail)?;
	let project_row = projects::service::get(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	let links = if ctx.perms.can(Action::Read, Resource::Application) {
		links_service::for_model(&s, ctx.user.id, ctx.team.id, project, model_id)
			.await
			.map_err(fail)?
	} else {
		ModelLinks {
			linked: Vec::new(),
			linkable: Vec::new(),
		}
	};
	let usage = links_service::usage(&s, ctx.user.id, ctx.team.id, project, model_id)
		.await
		.map_err(fail)?;
	let files =
		crate::model_files::service::get(&s, ctx.user.id, ctx.team.id, model.model_files_id)
			.await
			.ok();

	let body = html! {
		div class="flex flex-col gap-6" data-signals="{linkApp: '', linkDefault: false, _error: ''}" {
			div {
				h1 class="text-xl font-semibold" { (model.name) }
				p class="text-sm text-muted-foreground" {
					(project_row.project.name)
					@if let Some(f) = &files {
						" · " (f.repo) " · " (human_bytes(u64::try_from(f.total_bytes).unwrap_or(0)))
					}
				}
			}
			(card(&html! { (card_content(&status_region(&ctx, &model))) }))
			(facts_card(&ctx, &model))
			(playground::chat_card(&ctx, &model))
			(log_card(&ctx, &model))
			(links_card(&ctx, &model, &links))
			(usage_card(&usage))
		}
	};
	let name = model.name.clone();
	Ok(app_page(
		&ctx,
		&name,
		&crumbs(&ctx, vec![(name.as_str(), None)]),
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

/// The poll target: the status region, plus the engine log once it exists.
async fn status(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, model_id)): Ids,
	Query(q): Query<StatusQuery>,
) -> Response {
	match models_service::get(&s, ctx.user.id, ctx.team.id, project, model_id).await {
		Ok(model) => {
			let events = sse::patch(&status_region(&ctx, &model));
			if q.logs == 0 && model.container_id.is_some() {
				events
					.and(sse::patch(&log_card(&ctx, &model)))
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
fn act(ctx: &TeamCtx, result: Result<Model, ServerError>, done: &str) -> Response {
	match result {
		Ok(model) => sse::patch(&status_region(ctx, &model))
			.and(sse::toast(done))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

async fn start(State(s): State<AppState>, ctx: TeamCtx, Path((_, p, m)): Ids) -> Response {
	let r = models_service::start(&s, ctx.user.id, ctx.team.id, p, m).await;
	act(&ctx, r, "Starting")
}

async fn stop(State(s): State<AppState>, ctx: TeamCtx, Path((_, p, m)): Ids) -> Response {
	let r = models_service::stop(&s, ctx.user.id, ctx.team.id, p, m).await;
	act(&ctx, r, "Stopping")
}

async fn restart(State(s): State<AppState>, ctx: TeamCtx, Path((_, p, m)): Ids) -> Response {
	let r = models_service::restart(&s, ctx.user.id, ctx.team.id, p, m).await;
	act(&ctx, r, "Restarting")
}

async fn remove(State(s): State<AppState>, ctx: TeamCtx, Path((_, p, m)): Ids) -> Response {
	let r = models_service::remove(&s, ctx.user.id, ctx.team.id, p, m).await;
	act(&ctx, r, "Removing")
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
