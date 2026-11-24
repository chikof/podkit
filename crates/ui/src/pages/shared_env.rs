//! Shared variables: one page for the three scopes they can be set at. Team ones sit among
//! the settings; project and environment ones are reached from the project page and switch
//! between each other with tabs.
//!
//! A secret's value is write-only: the form sends it and nothing here (HTML, signal patch or
//! event) carries one back. A plain variable's value is shown on the page of its own scope,
//! and only there.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use crate::components::{
	CHECKBOX, INPUT, LABEL, Size, TEXTAREA, TabItem, Tone, Variant, badge, button_class, card,
	card_content, card_section, empty_state, icon, tabs,
};
use crate::extract::TeamCtx;
use crate::layout::{Crumb, app_page, form_error};
use crate::pages::apps::app_url;
use crate::pages::settings::{settings_body, settings_crumbs};
use crate::sse::js_string;
use crate::{error, sse};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::error::ServerError;
use podkit_core::ids::{EnvironmentId, ProjectId, SharedVarId, TeamId};
use podkit_core::shared_env::Scope;
use podkit_core::shared_env::service::{self, Affected, AffectedKind, ScopeInfo, SharedVarView};
use podkit_core::state::AppState;

const TEAM: &str = "/teams/{team_id}/settings/shared-variables";
const PROJECT: &str = "/teams/{team_id}/projects/{project_id}/shared-variables";
const ENVIRONMENT: &str =
	"/teams/{team_id}/projects/{project_id}/environments/{environment_id}/shared-variables";

pub fn router() -> Router<AppState> {
	let mut r = Router::new();
	for base in [TEAM, PROJECT, ENVIRONMENT] {
		r = r
			.route(base, get(page).post(set))
			.route(&format!("{base}/{{var_id}}/delete"), post(remove))
			.route(&format!("{base}/{{var_id}}/preview/{{on}}"), post(preview))
			.route(&format!("{base}/redeploy/{{key}}"), post(redeploy));
	}
	r
}

/// Every parameter any of the three routes can carry.
#[derive(Deserialize)]
struct Params {
	team_id: TeamId,
	project_id: Option<ProjectId>,
	environment_id: Option<EnvironmentId>,
	var_id: Option<SharedVarId>,
	key: Option<String>,
	on: Option<String>,
}

impl Params {
	fn scope(&self) -> Scope {
		match (self.project_id, self.environment_id) {
			(Some(p), Some(e)) => Scope::Environment(self.team_id, p, e),
			(Some(p), None) => Scope::Project(self.team_id, p),
			_ => Scope::Team(self.team_id),
		}
	}
}

fn base_url(scope: Scope) -> String {
	match scope {
		Scope::Team(t) => format!("/teams/{t}/settings/shared-variables"),
		Scope::Project(t, p) => format!("/teams/{t}/projects/{p}/shared-variables"),
		Scope::Environment(t, p, e) => {
			format!("/teams/{t}/projects/{p}/environments/{e}/shared-variables")
		}
	}
}

fn tab_links(team: TeamId, info: &ScopeInfo) -> Vec<(String, String, bool)> {
	let Some((project, _)) = &info.project else {
		return Vec::new();
	};
	let mut out = vec![(
		"Project".to_owned(),
		base_url(Scope::Project(team, *project)),
		info.environment.is_none(),
	)];
	for (id, name) in &info.environments {
		out.push((
			name.clone(),
			base_url(Scope::Environment(team, *project, *id)),
			info.environment.as_ref().is_some_and(|(e, _)| e == id),
		));
	}
	out
}

fn var_list(base: &str, vars: &[SharedVarView], can_delete: bool, can_mark: bool) -> Markup {
	html! {
		div id="shared-list" {
			@if vars.is_empty() {
				(empty_state(
					"key-round",
					"No shared variables here yet.",
					Some("Set a value once and every application and stack below inherits it. An application's own variable of the same name wins."),
					None,
				))
			} @else {
				ul class="ui-card-rows" aria-label="Shared variables" {
					@for var in vars {
						li class="ui-card-row" {
							div class="flex min-w-0 flex-col" {
								code class="font-mono text-sm font-medium break-all" { (var.key) }
								@if let Some(value) = &var.value {
									code class="font-mono text-xs text-muted-foreground break-all" { (value) }
								} @else {
									span class="text-xs text-muted-foreground" { "Value hidden" }
								}
							}
							div class="flex items-center gap-2" {
								@if var.is_secret { (badge(Tone::Neutral, "Secret")) }
								@if var.is_secret && var.preview { (badge(Tone::Neutral, "In previews")) }
								@if var.is_secret && can_mark {
									button type="button" class=(button_class(Variant::Outline, Size::Sm))
										"data-on:click"={ "@post(" (js_string(&format!("{base}/{}/preview/{}", var.id, if var.preview { "off" } else { "on" }))) ")" } {
										@if var.preview { "Withhold from previews" } @else { "Allow in previews" }
									}
								}
								@if can_delete {
									button type="button" class=(button_class(Variant::Ghost, Size::Icon)) aria-label={ "Delete " (var.key) }
										"data-on:click"={ "@post(" (js_string(&format!("{base}/{}/delete", var.id))) ")" } {
										(icon("trash-2"))
									}
								}
							}
						}
					}
				}
			}
		}
	}
}

/// What a change reaches, with the button that deploys it. Empty until something changed.
fn changed_slot(
	base: &str,
	team: TeamId,
	change: Option<(&str, &[Affected])>,
	can_redeploy: bool,
) -> Markup {
	html! {
		div id="shared-changed" {
			@if let Some((key, affected)) = change {
				div class="ui-shared-changed" role="status" {
					p {
						code class="font-mono" { (key) }
						@if affected.is_empty() {
							" changed. No application or stack inherits it yet."
						} @else {
							" changed. Running containers see it after their next deploy. It reaches "
							(affected.len()) @if affected.len() == 1 { " resource:" } @else { " resources:" }
						}
					}
					@if !affected.is_empty() {
						ul class="ui-shared-affected" aria-label="Affected resources" {
							@for item in affected {
								li {
									a class="hover:underline" href=(match item.kind {
										AffectedKind::Application => app_url(team, item.project, podkit_core::ids::ApplicationId(item.id)),
										AffectedKind::Stack => format!("/teams/{team}/projects/{}/stacks/{}", item.project, item.id),
									}) { (item.name) }
									span class="text-muted-foreground" {
										@match item.kind {
											AffectedKind::Application => { " · application" }
											AffectedKind::Stack => { " · stack" }
										}
									}
								}
							}
						}
						@if can_redeploy {
							div {
								button type="button" class=(button_class(Variant::Outline, Size::Sm))
									"data-on:click"={ "@post(" (js_string(&format!("{base}/redeploy/{key}"))) ")" } {
									"Redeploy affected"
								}
							}
						}
					}
				}
			}
		}
	}
}

#[allow(
	clippy::too_many_lines,
	reason = "one page of three scopes: head, cards and form"
)]
async fn render(s: &AppState, ctx: &TeamCtx, scope: Scope) -> Result<Markup, Response> {
	let info = service::context(&s.db, ctx.user.id, scope)
		.await
		.map_err(|e| error::page(ctx, &e))?;
	let vars = service::list(s, ctx.user.id, scope)
		.await
		.map_err(|e| error::page(ctx, &e))?;
	let base = base_url(scope);
	let can_create = ctx.perms.can(Action::Create, Resource::EnvVar);
	let can_delete = ctx.perms.can(Action::Delete, Resource::EnvVar);
	let can_mark = ctx.perms.can(Action::Update, Resource::EnvVar);
	let can_redeploy = ctx.perms.can(Action::Create, Resource::Deployment);

	let (what, inherits) = match scope {
		Scope::Team(_) => (
			"team",
			"Every application and stack in this team inherits these.",
		),
		Scope::Project(..) => (
			"project",
			"Every application and stack in this project inherits these. An environment's own shared variable of the same name wins.",
		),
		Scope::Environment(..) => (
			"environment",
			"Every application and stack in this environment inherits these.",
		),
	};
	let set = format!("evt.preventDefault(); @post({})", js_string(&base));
	let content = html! {
		div class="flex flex-col gap-6" data-signals=(json!({
			"sv_key": "", "sv_value": "", "sv_secret": true, "_error": "",
		}).to_string()) {
			(changed_slot(&base, ctx.team.id, None, can_redeploy))
			(card(&html! {
				(card_section(
					&format!("Shared variables ({what})"),
					Some(&format!("{inherits} Precedence, lowest first: team, project, environment, then the application's own. Reference one inside a value with ${{{{shared.NAME}}}}.")),
					None,
				))
				div class="ui-card-list" {
					div class="ui-card-slot" { (form_error()) }
					(var_list(&base, &vars, can_delete, can_mark))
				}
			}))
			@if can_create {
				(card(&html! {
					(card_section("Add or update a variable", None, None))
					(card_content(&html! {
						form class="flex flex-col gap-4" "data-on:submit"=(set) {
							label class=(LABEL) {
								"Name"
								input class={ (INPUT) " font-mono" } type="text" required maxlength="255" placeholder="STRIPE_KEY" autocomplete="off" spellcheck="false" "data-bind:sv_key";
							}
							label class=(LABEL) {
								"Value"
								textarea class=(TEXTAREA) rows="3" autocomplete="off" spellcheck="false" "data-bind:sv_value" {}
							}
							label class="ui-check" {
								input type="checkbox" class=(CHECKBOX) "data-bind:sv_secret";
								"Secret: hide the value once saved"
							}
							div {
								button type="submit" class=(button_class(Variant::Default, Size::Md)) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Save variable" }
							}
						}
					}))
				}))
			}
		}
	};

	let team = ctx.team.id;
	let body = match scope {
		Scope::Team(_) => settings_body(ctx, &content),
		Scope::Project(..) | Scope::Environment(..) => {
			let project_name = info.project.as_ref().map_or("", |(_, n)| n.as_str());
			let links = tab_links(team, &info);
			let items: Vec<TabItem<'_>> = links
				.iter()
				.map(|(label, href, current)| TabItem {
					label,
					href,
					current: *current,
				})
				.collect();
			html! {
				div class="flex min-w-0 flex-col gap-4" {
					header class="ui-page-head" {
						div class="ui-page-title" {
							p class="ui-eyebrow" { (project_name) }
							h1 { "Shared variables" }
							p class="ui-lede" {
								@if let Some((_, env)) = &info.environment {
									"Set once for the " (env) " environment."
								} @else {
									"Set once for every environment of this project."
								}
							}
						}
					}
					div class="min-w-0" { (tabs("Shared variables scope", &items)) }
					div class="min-w-0" { (content) }
				}
			}
		}
	};
	let crumbs: Vec<Crumb<'_>> = match scope {
		Scope::Team(_) => settings_crumbs(ctx, "Shared variables"),
		Scope::Project(..) | Scope::Environment(..) => {
			let (project, name) = info
				.project
				.as_ref()
				.map_or((ProjectId(0), ""), |(p, n)| (*p, n.as_str()));
			vec![
				("Projects", Some(format!("/teams/{team}/projects"))),
				(name, Some(format!("/teams/{team}/projects/{project}"))),
				("Shared variables", None),
			]
		}
	};
	Ok(app_page(ctx, "Shared variables", &crumbs, &body))
}

async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path(p): Path<Params>,
) -> Result<Markup, Response> {
	render(&s, &ctx, p.scope()).await
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SetSignals {
	#[serde(rename = "sv_key")]
	key: String,
	#[serde(rename = "sv_value")]
	value: String,
	#[serde(rename = "sv_secret")]
	secret: bool,
}

async fn changed(
	s: &AppState,
	ctx: &TeamCtx,
	scope: Scope,
	key: &str,
) -> Result<Markup, ServerError> {
	let affected = service::affected(&s.db, ctx.user.id, scope, key).await?;
	let can_redeploy = ctx.perms.can(Action::Create, Resource::Deployment);
	Ok(changed_slot(
		&base_url(scope),
		ctx.team.id,
		Some((key, &affected)),
		can_redeploy,
	))
}

async fn set(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path(p): Path<Params>,
	ReadSignals(sig): ReadSignals<SetSignals>,
) -> Response {
	let scope = p.scope();
	let key = sig.key.trim().to_owned();
	let result = async {
		service::set(&s, ctx.user.id, scope, &key, &sig.value, sig.secret).await?;
		let vars = service::list(&s, ctx.user.id, scope).await?;
		let slot = changed(&s, &ctx, scope, &key).await?;
		Ok::<_, ServerError>((vars, slot))
	}
	.await;
	match result {
		Ok((vars, slot)) => {
			let can_delete = ctx.perms.can(Action::Delete, Resource::EnvVar);
			sse::patch(&var_list(
				&base_url(scope),
				&vars,
				can_delete,
				ctx.perms.can(Action::Update, Resource::EnvVar),
			))
			.and(sse::patch(&slot))
			.and(sse::signals(
				&json!({ "sv_key": "", "sv_value": "", "_error": "" }),
			))
			.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn remove(State(s): State<AppState>, ctx: TeamCtx, Path(p): Path<Params>) -> Response {
	let scope = p.scope();
	let Some(id) = p.var_id else {
		return error::action(&ServerError::NotFound);
	};
	let result = async {
		let key = service::remove(&s, ctx.user.id, scope, id).await?;
		let vars = service::list(&s, ctx.user.id, scope).await?;
		let slot = changed(&s, &ctx, scope, &key).await?;
		Ok::<_, ServerError>((vars, slot))
	}
	.await;
	match result {
		Ok((vars, slot)) => {
			let can_delete = ctx.perms.can(Action::Delete, Resource::EnvVar);
			sse::patch(&var_list(
				&base_url(scope),
				&vars,
				can_delete,
				ctx.perms.can(Action::Update, Resource::EnvVar),
			))
			.and(sse::patch(&slot))
			.and(sse::signals(&json!({ "_error": "" })))
			.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn redeploy(State(s): State<AppState>, ctx: TeamCtx, Path(p): Path<Params>) -> Response {
	let scope = p.scope();
	let key = p.key.unwrap_or_default();
	match service::redeploy_affected(&s, ctx.user.id, scope, &key).await {
		Ok(done) => {
			let message = match (done.queued, done.skipped) {
				(0, 0) => "Nothing to redeploy.".to_owned(),
				(n, 0) => format!("Queued {n} deploy{}.", if n == 1 { "" } else { "s" }),
				(n, k) => {
					format!("Queued {n}; skipped {k} you can't deploy or that couldn't be queued.")
				}
			};
			sse::patch(&changed_slot(&base_url(scope), ctx.team.id, None, false))
				.and(sse::toast(&message))
				.and(sse::signals(&json!({ "_error": "" })))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

/// Marks a secret as available to previews of pull requests, or withholds it.
async fn preview(State(s): State<AppState>, ctx: TeamCtx, Path(p): Path<Params>) -> Response {
	let scope = p.scope();
	let (Some(id), Some(on)) = (p.var_id, p.on.as_deref()) else {
		return error::action(&ServerError::NotFound);
	};
	let available = match on {
		"on" => true,
		"off" => false,
		_ => return error::action(&ServerError::NotFound),
	};
	let result = async {
		service::set_preview(&s, ctx.user.id, scope, id, available).await?;
		service::list(&s, ctx.user.id, scope).await
	}
	.await;
	match result {
		Ok(vars) => {
			let can_delete = ctx.perms.can(Action::Delete, Resource::EnvVar);
			let can_mark = ctx.perms.can(Action::Update, Resource::EnvVar);
			sse::patch(&var_list(&base_url(scope), &vars, can_delete, can_mark)).into_response()
		}
		Err(e) => error::action(&e),
	}
}
