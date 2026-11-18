//! The create flow after the set-up: download the weights (live progress and log), then
//! start the engine. The stepper at the top says where in the flow the person is; the
//! first four steps were the set-up on the catalog.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;

use super::index::progress_bar;
use super::{catalog_url, crumbs, download_url, flow_steps, model_url, models_url};
use crate::auth::permission::{Action, Resource};
use crate::catalog::service as catalog_service;
use crate::catalog::{self, Recipe, suggest};
use crate::error::ServerError;
use crate::ids::{EnvironmentId, ModelFileId, ProjectId, ServerId, TeamId};
use crate::model_files::hf::Selection;
use crate::model_files::service::{self as files_service, CreateFile};
use crate::model_files::{ModelFile, ModelFileStatus};

use crate::models::engines::{self, EngineKind};
use crate::models::service::{self as models_service, CreateModel};
use crate::projects;
use crate::state::AppState;
use crate::ui::components::{
	Current, INPUT, LABEL, LogoSize, SELECT, Size, Variant, button_class, card, card_content,
	card_section, icon, logo, stepper,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::live::{job_log, log_view};
use crate::ui::sse::js_string;
use crate::ui::status::{Status, poll, status_badge};
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/models/downloads/recipe",
			post(download_recipe),
		)
		.route("/teams/{team_id}/models/downloads/hf", post(download_hf))
		.route("/teams/{team_id}/models/downloads/{file_id}", get(page))
		.route(
			"/teams/{team_id}/models/downloads/{file_id}/progress",
			get(progress),
		)
		.route("/teams/{team_id}/models/downloads/{file_id}/log", get(log))
		.route(
			"/teams/{team_id}/models/downloads/{file_id}/cancel",
			post(cancel),
		)
		.route(
			"/teams/{team_id}/models/downloads/{file_id}/retry",
			post(retry),
		)
		.route(
			"/teams/{team_id}/models/downloads/{file_id}/delete",
			post(remove),
		)
		.route(
			"/teams/{team_id}/models/downloads/{file_id}/start",
			post(start),
		)
}

/// What the start form is filled with. It travels in the query, so the page
/// can be reloaded and the choices made in the catalog survive the download.
#[derive(Debug, Default, Clone, Deserialize)]
struct Defaults {
	project: Option<String>,
	environment: Option<String>,
	name: Option<String>,
	engine: Option<String>,
	path: Option<String>,
	ctx: Option<String>,
	served: Option<String>,
}

impl Defaults {
	fn query(&self) -> String {
		query_of(&[
			("project", self.project.as_deref().unwrap_or_default()),
			(
				"environment",
				self.environment.as_deref().unwrap_or_default(),
			),
			("name", self.name.as_deref().unwrap_or_default()),
			("engine", self.engine.as_deref().unwrap_or_default()),
			("path", self.path.as_deref().unwrap_or_default()),
			("ctx", self.ctx.as_deref().unwrap_or_default()),
			("served", self.served.as_deref().unwrap_or_default()),
		])
	}
}

fn parse_engine(s: &str) -> Option<EngineKind> {
	serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
}

fn engine_key(engine: EngineKind) -> String {
	serde_json::to_value(engine)
		.ok()
		.and_then(|v| v.as_str().map(str::to_owned))
		.unwrap_or_default()
}

/// A `?query` of the non-empty values, percent-encoded.
fn query_of(pairs: &[(&str, &str)]) -> String {
	use std::fmt::Write as _;
	let mut out = String::new();
	for (key, value) in pairs.iter().filter(|(_, v)| !v.is_empty()) {
		out.push(if out.is_empty() { '?' } else { '&' });
		out.push_str(key);
		out.push('=');
		for b in value.bytes() {
			if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
				out.push(char::from(b));
			} else {
				let _ = write!(out, "%{b:02X}");
			}
		}
	}
	out
}

fn parse_server(s: &str) -> Result<ServerId, ServerError> {
	s.parse::<i64>()
		.map(ServerId)
		.map_err(|_| ServerError::Validation("Pick a server first.".to_owned()))
}

/// The download of the same files on the same server that is not failed, so a
/// second click opens it instead of failing as a duplicate.
async fn existing(s: &AppState, ctx: &TeamCtx, req: &CreateFile) -> Option<ModelFile> {
	let revision = req.revision.as_deref().unwrap_or("main");
	files_service::list(s, ctx.user.id, ctx.team.id)
		.await
		.ok()?
		.into_iter()
		.find(|f| {
			f.server_id == req.server_id
				&& f.repo == req.repo.trim()
				&& f.revision == revision
				&& f.selection.0 == req.selection
				&& f.status != ModelFileStatus::Failed
		})
}

async fn begin(s: &AppState, ctx: &TeamCtx, req: CreateFile, defaults: &Defaults) -> Response {
	let file = match existing(s, ctx, &req).await {
		Some(file) => file,
		None => match files_service::create(s, ctx.user.id, ctx.team.id, req).await {
			Ok(file) => file,
			Err(e) => return error::action(&e),
		},
	};
	sse::redirect(&format!(
		"{}{}",
		download_url(ctx.team.id, file.id),
		defaults.query()
	))
	.into_response()
}

fn find_recipe(
	catalog_id: &str,
	recipe_id: &str,
) -> Option<(&'static catalog::Model, &'static Recipe)> {
	let model = catalog::models().iter().find(|m| m.id == catalog_id)?;
	let recipe = model.recipes.iter().find(|r| r.id == recipe_id)?;
	Some((model, recipe))
}

#[derive(Deserialize)]
struct RecipeSignals {
	catalog: String,
	recipe: String,
	server: String,
	project: String,
	#[serde(default)]
	environment: String,
	name: String,
	#[serde(default)]
	served: String,
	#[serde(default)]
	ctx: String,
}

async fn download_recipe(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<RecipeSignals>,
) -> Response {
	let server = match parse_server(&sig.server) {
		Ok(id) => id,
		Err(e) => return error::action(&e),
	};
	let Some((model, recipe)) = find_recipe(&sig.catalog, &sig.recipe) else {
		return sse::error("Pick a recipe.").into_response();
	};
	// The engine to run comes from the same verdict the catalog showed.
	let servers = match catalog_service::team_servers(&s, ctx.user.id, ctx.team.id).await {
		Ok(servers) => servers,
		Err(e) => return error::action(&e),
	};
	let engine = servers
		.iter()
		.find(|srv| srv.id == server)
		.and_then(|srv| srv.hardware.as_ref())
		.and_then(|hw| {
			suggest(hw, model)
				.into_iter()
				.find(|(r, _)| r.id == recipe.id)
				.and_then(|(_, fit)| fit.engine)
		})
		.unwrap_or(recipe.engine);
	let path = if recipe.model_path == "." {
		String::new()
	} else {
		recipe.model_path.clone()
	};
	let req = CreateFile {
		server_id: server,
		repo: recipe.repo.clone(),
		revision: Some(recipe.revision.clone()),
		selection: recipe.selection(),
	};
	let defaults = Defaults {
		project: Some(sig.project),
		environment: Some(sig.environment),
		name: Some(sig.name),
		engine: Some(engine_key(engine)),
		path: Some(path),
		ctx: Some(sig.ctx),
		served: Some(sig.served),
	};
	begin(&s, &ctx, req, &defaults).await
}

#[derive(Deserialize)]
struct HfSignals {
	server: String,
	#[serde(rename = "hfRepo")]
	repo: String,
	#[serde(rename = "hfPath")]
	path: String,
	#[serde(rename = "hfSelection")]
	selection: String,
	#[serde(rename = "hfEngine", default)]
	engine: String,
}

async fn download_hf(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<HfSignals>,
) -> Response {
	let server = match parse_server(&sig.server) {
		Ok(id) => id,
		Err(e) => return error::action(&e),
	};
	let Ok(selection) = serde_json::from_str::<Selection>(&sig.selection) else {
		return sse::error("Pick a quantization to download.").into_response();
	};
	let req = CreateFile {
		server_id: server,
		repo: sig.repo,
		revision: None,
		selection,
	};
	let name = req.repo.rsplit('/').next().unwrap_or_default().to_owned();
	let defaults = Defaults {
		name: Some(name),
		engine: Some(sig.engine),
		path: Some(sig.path),
		..Defaults::default()
	};
	begin(&s, &ctx, req, &defaults).await
}

fn region_url(team: TeamId, file: ModelFileId, defaults: &Defaults) -> String {
	format!("{}/progress{}", download_url(team, file), defaults.query())
}

/// The part of the page that changes while the download runs and when it
/// ends; polling replaces it as a whole.
async fn region(
	s: &AppState,
	ctx: &TeamCtx,
	file: &ModelFile,
	defaults: &Defaults,
) -> Result<Markup, ServerError> {
	let base = download_url(ctx.team.id, file.id);
	let can_update = ctx.perms.can(Action::Update, Resource::ModelFile);
	let can_delete = ctx.perms.can(Action::Delete, Resource::ModelFile);
	let ready = file.status == ModelFileStatus::Ready;
	let form = if ready && ctx.perms.can(Action::Create, Resource::Model) {
		Some(start_form(s, ctx, file, defaults).await?)
	} else {
		None
	};
	Ok(html! {
		div id="download" class="flex flex-col gap-6" {
			(stepper("Create a model", &flow_steps(), &Current::At(if ready { 6 } else { 5 })))
			(card(&html! {
				(card_section("Download", Some("Weights are fetched from Hugging Face onto the server. An interrupted download resumes."), None))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						div class="flex flex-wrap items-center justify-between gap-3" {
							span class="flex min-w-0 items-center gap-3" {
								(logo("huggingface", LogoSize::Sm))
								span class="text-sm font-medium break-all" { (file.repo) " @ " (file.revision.chars().take(8).collect::<String>()) }
							}
							(status_badge(file.status))
						}
						(progress_bar(file))
						@if let Some(error) = &file.error {
							p class="text-sm text-destructive" role="alert" { (error) }
						}
						div class="flex flex-wrap gap-3" {
							@if can_update && file.status.in_flight() {
								button type="button" class=(button_class(Variant::Outline, Size::Md)) "data-on:click"={ "@post(" (js_string(&format!("{base}/cancel"))) ")" } { "Cancel" }
							}
							@if can_update && file.status == ModelFileStatus::Failed {
								button type="button" class=(button_class(Variant::Default, Size::Md)) "data-on:click"={ "@post(" (js_string(&format!("{base}/retry{}", defaults.query()))) ")" } { (icon("refresh-cw")) "Retry" }
							}
							@if can_delete && !file.status.in_flight() {
								button type="button" class=(button_class(Variant::Destructive, Size::Md)) "data-on:click"={ "@post(" (js_string(&format!("{base}/delete"))) ")" } { (icon("trash-2")) "Delete files" }
							}
						}
					}
				}))
			}))
			@if let Some(form) = form { (form) }
			(poll(&region_url(ctx.team.id, file.id, defaults), file.status.in_flight()))
		}
	})
}

async fn start_form(
	s: &AppState,
	ctx: &TeamCtx,
	file: &ModelFile,
	defaults: &Defaults,
) -> Result<Markup, ServerError> {
	let projects = projects::service::list(&s.db, ctx.user.id, ctx.team.id).await?;
	let chosen = defaults
		.project
		.as_deref()
		.and_then(|p| p.parse::<i64>().ok())
		.filter(|id| projects.iter().any(|p| p.project.id.0 == *id))
		.or_else(|| projects.first().map(|p| p.project.id.0));
	let engine = defaults
		.engine
		.as_deref()
		.and_then(parse_engine)
		.filter(|e| *e != EngineKind::Custom)
		.map_or_else(String::new, engine_key);
	let signals = format!(
		"{{project: {}, environment: {}, name: {}, engine: {}, path: {}, ctx: {}, served: {}, _error: ''}}",
		js_string(&chosen.map_or(String::new(), |id| id.to_string())),
		js_string(
			&defaults
				.environment
				.as_deref()
				.and_then(|e| e.parse::<i64>().ok())
				.map_or(String::new(), |e| e.to_string())
		),
		js_string(defaults.name.as_deref().unwrap_or(&file.repo)),
		js_string(&engine),
		js_string(defaults.path.as_deref().unwrap_or_default()),
		js_string(defaults.ctx.as_deref().unwrap_or("8192")),
		js_string(defaults.served.as_deref().unwrap_or_default()),
	);
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{}/start", download_url(ctx.team.id, file.id)))
	);
	Ok(html! {
		div data-signals=(signals) {
			(card(&html! {
				(card_section("Start", Some("The files are on the server. Choose how to run them."), None))
				(card_content(&html! {
					form class="flex flex-col gap-4" "data-on:submit"=(post) {
						label class=(LABEL) {
							"Project"
							select class=(SELECT) required "data-bind:project" {
								@for p in &projects { option value=(p.project.id.0) { (p.project.name) } }
							}
						}
						label class=(LABEL) {
							"Name"
							input class=(INPUT) type="text" required maxlength="60" "data-bind:name";
						}
						label class=(LABEL) {
							"Engine"
							select class=(SELECT) required "data-bind:engine" {
								@for spec in engines::all().iter().filter(|e| e.kind != EngineKind::Custom) {
									option value=(engine_key(spec.kind)) { (spec.label) }
								}
							}
						}
						details class="text-sm" {
							summary class="cursor-pointer font-medium select-none" { "Overrides" }
							div class="mt-4 flex flex-col gap-4" {
								label class=(LABEL) {
									"File or directory inside the download"
									input class=(INPUT) type="text" maxlength="500" placeholder="found from the files" "data-bind:path";
								}
								label class=(LABEL) {
									"Context size (tokens)"
									input class=(INPUT) type="number" min="256" max="1048576" "data-bind:ctx";
								}
								label class=(LABEL) {
									"Name clients send as the model"
									input class=(INPUT) type="text" maxlength="100" placeholder="defaults to the slug" "data-bind:served";
								}
							}
						}
						(form_error())
						div {
							button type="submit" class=(button_class(Variant::Default, Size::Md)) "data-indicator:_busy" "data-attr:disabled"="$_busy" { (icon("play")) "Start the model" }
						}
					}
				}))
			}))
		}
	})
}

async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, file_id)): Path<(TeamId, ModelFileId)>,
	Query(defaults): Query<Defaults>,
) -> Result<Response, Response> {
	let file = files_service::get(&s, ctx.user.id, ctx.team.id, file_id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let body_region = region(&s, &ctx, &file, &defaults)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let log_url = format!("{}/log", download_url(ctx.team.id, file.id));
	let body = html! {
		div class="flex flex-col gap-6" {
			div class="flex flex-col gap-1" {
				h1 class="text-2xl font-semibold tracking-tight" { "Download " (file.repo) }
				p class="text-sm text-muted-foreground" { "Then start it as a model in one of your projects." }
			}
			(body_region)
			(card(&html! {
				(card_section("Log", None, None))
				(card_content(&log_view("download-log", &log_url)))
			}))
		}
	};
	Ok(app_page(
		&ctx,
		"Download",
		&crumbs(
			&ctx,
			vec![
				("Catalog", Some(catalog_url(ctx.team.id))),
				("Download", None),
			],
		),
		&body,
	)
	.into_response())
}

async fn progress(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, file_id)): Path<(TeamId, ModelFileId)>,
	Query(defaults): Query<Defaults>,
) -> Response {
	let result = async {
		let file = files_service::get(&s, ctx.user.id, ctx.team.id, file_id).await?;
		region(&s, &ctx, &file, &defaults).await
	}
	.await;
	match result {
		Ok(markup) => sse::patch(&markup).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn log(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, file_id)): Path<(TeamId, ModelFileId)>,
) -> Response {
	match files_service::latest_job(&s, ctx.user.id, ctx.team.id, file_id).await {
		Ok(job) => job_log(&s, job, "download-log"),
		Err(e) => error::action(&e),
	}
}

async fn cancel(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, file_id)): Path<(TeamId, ModelFileId)>,
) -> Response {
	match files_service::cancel(&s, ctx.user.id, ctx.team.id, file_id).await {
		Ok(_) => sse::toast("Download cancelled. Retry resumes where it stopped.").into_response(),
		Err(e) => error::action(&e),
	}
}

async fn retry(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, file_id)): Path<(TeamId, ModelFileId)>,
	Query(defaults): Query<Defaults>,
) -> Response {
	match files_service::retry(&s, ctx.user.id, ctx.team.id, file_id).await {
		Ok(file) => sse::redirect(&format!(
			"{}{}",
			download_url(ctx.team.id, file.id),
			defaults.query()
		))
		.into_response(),
		Err(e) => error::action(&e),
	}
}

async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, file_id)): Path<(TeamId, ModelFileId)>,
) -> Response {
	match files_service::remove(&s, ctx.user.id, ctx.team.id, file_id).await {
		Ok(_) => sse::redirect(&models_url(ctx.team.id)).into_response(),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize)]
struct StartSignals {
	project: String,
	#[serde(default)]
	environment: String,
	name: String,
	engine: String,
	#[serde(default)]
	path: String,
	#[serde(default)]
	ctx: String,
	#[serde(default)]
	served: String,
}

fn non_empty(s: &str) -> Option<String> {
	let s = s.trim();
	(!s.is_empty()).then(|| s.to_owned())
}

async fn start(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, file_id)): Path<(TeamId, ModelFileId)>,
	ReadSignals(sig): ReadSignals<StartSignals>,
) -> Response {
	let result = async {
		let project = sig
			.project
			.parse::<i64>()
			.map(ProjectId)
			.map_err(|_| ServerError::Validation("Pick a project.".to_owned()))?;
		let environment = match non_empty(&sig.environment) {
			Some(text) => Some(text.parse::<i64>().map(EnvironmentId).map_err(|_| {
				ServerError::Validation("That environment doesn't exist.".to_owned())
			})?),
			None => None,
		};
		let engine = parse_engine(&sig.engine)
			.filter(|e| *e != EngineKind::Custom)
			.ok_or_else(|| ServerError::Validation("Pick an engine.".to_owned()))?;
		let context_size = match non_empty(&sig.ctx) {
			Some(text) => Some(text.parse::<u32>().map_err(|_| {
				ServerError::Validation("Context size is a number of tokens.".to_owned())
			})?),
			None => None,
		};
		let model = models_service::create(
			&s,
			ctx.user.id,
			ctx.team.id,
			project,
			CreateModel {
				name: sig.name.trim().to_owned(),
				model_files_id: file_id,
				engine,
				served_name: non_empty(&sig.served),
				model_path: non_empty(&sig.path),
				context_size,
				gpu: None,
				extra_args: Vec::new(),
				image: None,
				port: None,
				memory_limit_mb: None,
				environment_id: environment,
			},
		)
		.await?;
		Ok::<_, ServerError>(model_url(ctx.team.id, project, model.id))
	}
	.await;
	match result {
		Ok(url) => sse::redirect(&url).into_response(),
		Err(e) => error::action(&e),
	}
}

#[cfg(test)]
mod unit {
	use super::*;

	#[test]
	fn the_defaults_query_is_encoded_and_skips_empty_values() {
		let d = Defaults {
			name: Some("Qwen 3 & co".to_owned()),
			engine: Some("llama_cuda".to_owned()),
			path: Some(String::new()),
			environment: Some("7".to_owned()),
			..Defaults::default()
		};
		assert_eq!(
			d.query(),
			"?environment=7&name=Qwen%203%20%26%20co&engine=llama_cuda"
		);
		assert_eq!(Defaults::default().query(), "");
		assert_eq!(parse_engine("llama_cuda"), Some(EngineKind::LlamaCuda));
		assert_eq!(parse_engine("nope"), None);
	}
}
