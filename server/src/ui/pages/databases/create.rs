//! Creating a database: an engine picker (logo cards), then a version, a server, the public
//! port and the limits. A Configure surface: one page with a few sections, validation next to
//! the field (`:user-invalid`) and the server's reasons in one line above the buttons.
//!
//! `?engine=` preselects the engine and `?environment=<id>` carries the environment the
//! database is made in (the picker on the environment page links with both).

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::{
	ENGINES, crumbs, database_url, databases_url, engine_label, engine_value, id, lenient, number,
	parse_engine, project_url,
};
use crate::databases::service::{self, CreateInput};
use crate::error::ServerError;
use crate::ids::{EnvironmentId, ProjectId, ServerId, TeamId};
use crate::projects;
use crate::servers::ServerStatus;
use crate::state::AppState;
use crate::ui::components::{
	INPUT, LABEL, LogoSize, SELECT, Size, Variant, button_class, card, card_content, card_section,
	icon, link_button, logo,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::sse::js_string;
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/projects/{project_id}/databases",
			post(create),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/databases/new",
			get(new_page),
		)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CreateSignals {
	name: String,
	engine: String,
	version: String,
	server_id: String,
	#[serde(deserialize_with = "lenient")]
	public_port: String,
	#[serde(deserialize_with = "lenient")]
	memory_limit_mb: String,
	#[serde(deserialize_with = "lenient")]
	cpu_limit: String,
	#[serde(deserialize_with = "lenient")]
	environment: String,
}

impl CreateSignals {
	fn input(&self) -> Result<CreateInput, ServerError> {
		let version = self.version.trim();
		Ok(CreateInput {
			name: self.name.trim().to_owned(),
			engine: parse_engine(&self.engine)?,
			version: (!version.is_empty()).then(|| version.to_owned()),
			server_id: id(&self.server_id, "server", ServerId)?
				.ok_or_else(|| ServerError::Validation("Pick a server.".to_owned()))?,
			public_port: number(&self.public_port, "Public port")?,
			memory_limit_mb: number(&self.memory_limit_mb, "Memory limit")?,
			cpu_limit: number(&self.cpu_limit, "CPU limit")?,
			environment_id: id(&self.environment, "environment", EnvironmentId)?,
		})
	}
}

/// What each engine is for, on its card.
const fn engine_blurb(engine: crate::databases::EngineKind) -> &'static str {
	use crate::databases::EngineKind;
	match engine {
		EngineKind::Postgres => "Relational, the usual default.",
		EngineKind::Mysql => "Relational, widely hosted.",
		EngineKind::Mongodb => "Document store.",
		EngineKind::Redis => "In-memory cache and queue.",
	}
}

/// The version hint of one engine, shown while that engine is picked.
fn version_hint(option: &service::EngineOption) -> Markup {
	let condition = format!("$engine === {}", js_string(engine_value(option.kind)));
	html! {
		p class="text-xs font-normal text-muted-foreground" "data-show"=(condition) style="display: none" {
			"Default " strong { (option.default_version) }
			". Available: " (option.versions.join(", ")) "."
		}
	}
}

#[derive(Deserialize, Default)]
struct NewQuery {
	engine: Option<String>,
	environment: Option<i64>,
}

fn engine_picker(selected: &str) -> Markup {
	html! {
		fieldset class="ui-choice-grid" {
			legend class="ui-visually-hidden" { "Engine" }
			@for engine in ENGINES {
				@let value = engine_value(engine);
				label class="ui-choice" {
					input type="radio" name="engine" value=(value) checked[value == selected] "data-bind:engine";
					span class="ui-choice-check" aria-hidden="true" { (icon("circle-check")) }
					(logo(value, LogoSize::Md))
					span class="flex flex-col gap-0.5" {
						span class="text-sm font-medium" { (engine_label(engine)) }
						span class="text-xs text-muted-foreground" { (engine_blurb(engine)) }
					}
				}
			}
		}
	}
}

// A maud template reads best as one piece: the form, section by section.
#[allow(clippy::too_many_lines)]
async fn new_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	Query(q): Query<NewQuery>,
) -> Result<Markup, Response> {
	let fail = |e: ServerError| error::page(&ctx, &e);
	let project = projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	let servers = service::servers_for_create(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(fail)?;
	let servers: Vec<_> = servers
		.into_iter()
		.filter(|server| server.status == ServerStatus::Active)
		.collect();
	let options = service::engine_options();

	let engine = q
		.engine
		.as_deref()
		.and_then(|raw| parse_engine(raw).ok())
		.map_or("postgres", engine_value);
	let environment = q.environment.map(|e| e.to_string()).unwrap_or_default();
	let back = if environment.is_empty() {
		project_url(ctx.team.id, project.id)
	} else {
		format!(
			"{}?environment={environment}",
			project_url(ctx.team.id, project.id)
		)
	};
	let first = servers
		.first()
		.map(|server| server.id.to_string())
		.unwrap_or_default();
	let signals = json!({
		"name": "", "engine": engine, "version": "", "server_id": first,
		"public_port": "", "memory_limit_mb": "", "cpu_limit": "", "environment": environment,
		"_error": "",
	})
	.to_string();
	let post = format!(
		"evt.preventDefault(); @post({})",
		js_string(&databases_url(ctx.team.id, project.id))
	);

	let body = html! {
		div class="flex flex-col gap-6" "data-signals"=(signals) {
			div class="flex flex-col gap-1" {
				h1 class="text-2xl font-semibold tracking-tight" { "New database" }
				p class="text-sm text-muted-foreground" { "A container on one of your servers, with generated credentials." }
			}
			@if servers.is_empty() {
				(card(&html! {
					(card_section("No active server yet", Some("A database runs on one of your servers. Add one first, then come back."), None))
				}))
			} @else {
				form class="flex flex-col gap-6" "data-on:submit"=(post) {
					(card(&html! {
						(card_section("Engine", None, None))
						(card_content(&engine_picker(engine)))
					}))
					(card(&html! {
						(card_section("Database", None, None))
						(card_content(&html! {
							div class="flex flex-col gap-4" {
								label class=(LABEL) {
									"Name"
									input class=(INPUT) type="text" required maxlength="60" autocomplete="off" "data-bind:name" autofocus;
								}
								label class=(LABEL) {
									"Version (optional)"
									input class=(INPUT) type="text" autocomplete="off" placeholder="Engine default"
										"data-bind:version" "data-attr:list"="'versions-' + $engine";
									@for option in &options { (version_hint(option)) }
								}
								@for option in &options {
									datalist id={ "versions-" (engine_value(option.kind)) } {
										@for version in option.versions { option value=(version) {} }
									}
								}
								label class=(LABEL) {
									"Server"
									select class=(SELECT) "data-bind:server_id" {
										button { selectedcontent {} }
										@for server in &servers {
											option value=(server.id.0) { (server.name) }
										}
									}
								}
							}
						}))
					}))
					(card(&html! {
						(card_section("Network and limits", Some("All optional. You can change them later; the container is recreated and its data is kept."), None))
						(card_content(&html! {
							div class="flex flex-col gap-4" {
								label class=(LABEL) {
									"Public port"
									input class=(INPUT) type="text" inputmode="numeric" placeholder="Not published" "data-bind:public_port";
									span class="text-xs font-normal text-muted-foreground" { "Publish the database on the server's host at this port, 1024 to 65535. Leave empty to keep it reachable only by your applications." }
								}
								div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
									label class=(LABEL) {
										"Memory limit in MB"
										input class=(INPUT) type="text" inputmode="numeric" placeholder="No limit" "data-bind:memory_limit_mb";
									}
									label class=(LABEL) {
										"CPU limit in cores"
										input class=(INPUT) type="text" inputmode="decimal" placeholder="No limit" "data-bind:cpu_limit";
									}
								}
							}
						}))
					}))
					(form_error())
					div class="flex flex-wrap items-center justify-end gap-2" {
						(link_button(Variant::Outline, Size::Md, &back, &html! { "Cancel" }))
						button type="submit" class=(button_class(Variant::Default, Size::Md))
							"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Create database" }
					}
				}
			}
		}
	};
	Ok(app_page(
		&ctx,
		"New database",
		&crumbs(&ctx, &project, "New database"),
		&body,
	))
}

async fn create(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<CreateSignals>,
) -> Response {
	let result = async {
		let input = sig.input()?;
		service::create(&s, ctx.user.id, ctx.team.id, project, input).await
	}
	.await;
	match result {
		Ok(database) => {
			sse::redirect(&database_url(ctx.team.id, project, database.id)).into_response()
		}
		Err(e) => error::action(&e),
	}
}
