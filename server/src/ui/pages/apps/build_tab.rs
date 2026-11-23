//! The Build tab: how this application's image is built. Its strategy, whether a build may
//! reuse the layers of earlier ones, and which server builds it. Applies from the next deploy.
//!
//! An application that runs a registry image has nothing to build, and the tab says so. The
//! service authorizes and scopes: reading needs `read application`, saving `update
//! application`; someone who may not save sees the same facts as text.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::form::HELP;
use super::{AppPath, Tab, app_url, load, page, toast_ok};
use crate::applications::BuildStrategy;
use crate::applications::service::ApplicationView;
use crate::auth::permission::{Action, Resource};
use crate::builds::service::{self, AppInput, AppView};
use crate::ids::{ApplicationId, ProjectId, TeamId};
use crate::servers::ServerStatus;
use crate::state::AppState;
use crate::ui::components::{
	CHECKBOX, INPUT, LABEL, SELECT, card, card_content, card_section, empty_state, save_bar,
	save_bar_saved,
};
use crate::ui::extract::TeamCtx;
use crate::ui::sse::js_string;
use crate::ui::{error, sse};

/// The signals of the form; the save bar watches them.
const FIELDS: &[&str] = &["b_server", "b_cache", "b_strategy", "b_dockerfile"];

pub fn router() -> Router<AppState> {
	Router::new().route(
		"/teams/{team_id}/projects/{project_id}/apps/{app_id}/build",
		get(build_page).post(save),
	)
}

#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(
	clippy::struct_field_names,
	reason = "the names are the form's signals, which share the b_ prefix"
)]
struct Signals {
	b_server: String,
	b_cache: bool,
	b_strategy: String,
	b_dockerfile: String,
}

fn strategy_word(strategy: BuildStrategy, path: &str) -> String {
	match strategy {
		BuildStrategy::Dockerfile => format!("Dockerfile ({path})"),
		BuildStrategy::Nixpacks => "Nixpacks".to_owned(),
	}
}

/// Who builds when the application does not choose: the team's server, or its own.
fn team_default(view: &AppView, app: &ApplicationView) -> String {
	let named = |id| {
		view.servers
			.iter()
			.find(|s| s.id == id)
			.map_or_else(|| "the server it runs on".to_owned(), |s| s.name.clone())
	};
	named(view.team_server.unwrap_or(app.server_id))
}

fn builder_name(view: &AppView, app: &ApplicationView) -> String {
	match view.settings.build_server_id {
		Some(id) => view
			.servers
			.iter()
			.find(|s| s.id == id)
			.map_or_else(|| "a removed server".to_owned(), |s| s.name.clone()),
		None => format!("{} (the team's setting)", team_default(view, app)),
	}
}

fn summary(view: &AppView, app: &ApplicationView) -> Markup {
	html! {
		(card(&html! {
			(card_section("Build", None, None))
			(card_content(&html! {
				dl class="grid grid-cols-1 gap-x-6 gap-y-4 text-sm sm:grid-cols-3" {
					div class="flex flex-col gap-1" {
						dt class="text-muted-foreground" { "Strategy" }
						dd class="m-0 font-medium" { (strategy_word(view.strategy, &view.dockerfile_path)) }
					}
					div class="flex flex-col gap-1" {
						dt class="text-muted-foreground" { "Layer cache" }
						dd class="m-0 font-medium" { @if view.settings.cache_enabled { "On" } @else { "Off" } }
					}
					div class="flex flex-col gap-1" {
						dt class="text-muted-foreground" { "Built on" }
						dd class="m-0 font-medium break-words" { (builder_name(view, app)) }
					}
				}
			}))
		}))
	}
}

fn form(base: &str, view: &AppView, app: &ApplicationView) -> Markup {
	let signals = json!({
		"b_server": view.settings.build_server_id.map(|s| s.0.to_string()).unwrap_or_default(),
		"b_cache": view.settings.cache_enabled,
		"b_strategy": view.strategy.as_str(),
		"b_dockerfile": view.dockerfile_path,
		"_error": "",
	});
	let save = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/build"))
	);
	html! {
		form class="flex flex-col gap-6" data-signals=(signals.to_string()) "data-on:submit"=(save) {
			(card(&html! {
				(card_section("Build", Some("Applies from the next deploy."), None))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						label class=(LABEL) {
							"Build strategy"
							select class=(SELECT) "data-bind:b_strategy" {
								button { selectedcontent {} }
								option value=(BuildStrategy::Dockerfile.as_str()) { "Dockerfile" }
								option value=(BuildStrategy::Nixpacks.as_str()) { "Nixpacks (detect automatically)" }
							}
						}
						label class=(LABEL) "data-show"="$b_strategy === 'dockerfile'" {
							"Dockerfile path"
							input class={ (INPUT) " font-mono" } type="text" placeholder="Dockerfile" autocomplete="off"
								autocapitalize="none" spellcheck="false" "data-bind:b_dockerfile";
							span class=(HELP) { "Relative to the repository root." }
						}
						label class="flex items-start gap-3 text-sm font-normal" {
							input type="checkbox" class=(CHECKBOX) "data-bind:b_cache";
							span class="flex flex-col gap-1" {
								span class="font-medium" { "Reuse layers from earlier builds" }
								span class=(HELP) {
									"A build then repeats only the steps whose inputs changed. Turn it off to build every step again, "
									"for example after a base image changed under a tag."
								}
							}
						}
						label class=(LABEL) {
							"Build on"
							select class=(SELECT) "data-bind:b_server" {
								button { selectedcontent {} }
								option value="" { "The team's setting (" (team_default(view, app)) ")" }
								@for server in view.servers.iter().filter(|s| s.status == ServerStatus::Active) {
									option value=(server.id) { (server.name) }
								}
							}
							span class=(HELP) {
								"A build on another server is sent to " (app.server_name) " when it is done, so it does not slow that server down."
							}
						}
					}
				}))
			}))
			(save_bar(FIELDS))
		}
	}
}

async fn build_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	path: AppPath,
) -> Result<Markup, Response> {
	let Path((_, project, app)) = path;
	let (project, app) = load(&s, &ctx, project, app).await?;
	let view = service::app_view(&s, ctx.user.id, ctx.team.id, project.id, app.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let base = app_url(ctx.team.id, project.id, app.id);
	let content = if !view.git {
		html! {
			(card(&html! {
				(card_content(&empty_state(
					"package",
					"Nothing to build",
					Some("This application runs a registry image, so there is no build. Pulling it is part of the deploy."),
					None,
				)))
			}))
		}
	} else if ctx.perms.can(Action::Update, Resource::Application) {
		form(&base, &view, &app)
	} else {
		summary(&view, &app)
	};
	Ok(page(&s, &ctx, &project, &app, Tab::Build, &content).await)
}

async fn save(
	State(s): State<AppState>,
	ctx: TeamCtx,
	path: AppPath,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)> = path;
	let input = AppInput {
		build_server: sig.b_server,
		cache: sig.b_cache,
		strategy: sig.b_strategy,
		dockerfile_path: sig.b_dockerfile,
	};
	match service::save_app(&s, ctx.user.id, ctx.team.id, project, app, &input).await {
		Ok(()) => sse::signals(&json!({ "_error": "" }))
			.and(toast_ok("Build settings saved"))
			.and(save_bar_saved())
			.into_response(),
		Err(e) => error::action(&e),
	}
}
