//! The deploy dialog: project, environment, server, name, the template's questions, Deploy.
//!
//! It posts the form as signals to `templates::service::deploy`, which authorizes, scopes
//! (project, environment and server must all be the team's), fills in the variables and
//! creates an image application or a stack. On success the browser goes to the new
//! resource's live log. A template may list generated credentials the user logs in with
//! (`reveal`); those come back once, in this response, so the dialog shows them in place of
//! redirecting, with a link to the new resource. Nothing else generated is ever shown.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::{Value, json};

use super::Place;
use crate::environments::{self, Environment};
use crate::error::ServerError;
use crate::ids::{ApplicationId, EnvironmentId, ProjectId, ServerId, TeamId};
use crate::projects::Project;
use crate::servers::{self, Server, ServerStatus};
use crate::state::AppState;
use crate::templates::service::{DeployInput, Deployed, Kind};
use crate::templates::{self, Loaded, VariableInfo};
use crate::ui::components::{
	BUTTON, BUTTON_OUTLINE, INPUT, LABEL, SELECT, Size, Variant, icon, link_button,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::form_error;
use crate::ui::sse::{self, js_string};
use crate::ui::{error, pages};

pub(super) const DIALOG_ID: &str = "tpl-deploy";

/// What the dialog offers to pick from.
pub(super) struct Targets {
	projects: Vec<Project>,
	project: Option<ProjectId>,
	environments: Vec<Environment>,
	environment: Option<EnvironmentId>,
	servers: Vec<Server>,
}

pub(super) async fn load_targets(
	s: &AppState,
	ctx: &TeamCtx,
	place: Place,
) -> Result<Targets, ServerError> {
	let projects: Vec<Project> = crate::projects::service::list(&s.db, ctx.user.id, ctx.team.id)
		.await?
		.into_iter()
		.map(|p| p.project)
		.collect();
	let project = place
		.project
		.filter(|p| projects.iter().any(|x| x.id == *p))
		.or_else(|| projects.first().map(|p| p.id));
	let environments = match project {
		Some(p) => environments::service::list(&s.db, ctx.user.id, ctx.team.id, p).await?,
		None => Vec::new(),
	};
	let environment = environments
		.iter()
		.find(|e| Some(e.id.0) == place.environment)
		.or_else(|| environments.first())
		.map(|e| e.id);
	let servers = servers::service::list(&s.db, ctx.user.id, ctx.team.id)
		.await?
		.into_iter()
		.filter(|x| x.status == ServerStatus::Active)
		.collect();
	Ok(Targets {
		projects,
		project,
		environments,
		environment,
		servers,
	})
}

/// The answers the form can take: inputs and hostnames, in the order the page lists them.
/// The field `v<i>` of the form is the i-th of these, so a variable's name never has to be
/// a signal name.
fn asked(vars: &[VariableInfo]) -> Vec<&VariableInfo> {
	vars.iter().filter(|v| v.kind != "generated").collect()
}

fn environment_field(environments: &[Environment], selected: Option<EnvironmentId>) -> Markup {
	html! {
		label id="tpl-env-field" class=(LABEL) {
			"Environment"
			select class=(SELECT) required "data-bind:environment" {
				button { selectedcontent {} }
				@for e in environments {
					option value=(e.id.0) selected[Some(e.id) == selected] { (e.name) }
				}
			}
		}
	}
}

fn var_field(i: usize, v: &VariableInfo) -> Markup {
	let title = v
		.label
		.clone()
		.unwrap_or_else(|| v.name.replace(['_', '-'], " "));
	let bind = format!("v{i}");
	let help_id = format!("tpl-var-{i}-help");
	let placeholder = if v.kind == "domain" {
		"Leave empty for a free hostname"
	} else {
		""
	};
	html! {
		div class="ui-field-group" {
			label for=(format!("tpl-var-{i}")) class="ui-label" {
				(title)
				@if v.kind == "input" && !v.required { span class="font-normal text-muted-foreground" { " (optional)" } }
			}
			input id=(format!("tpl-var-{i}")) class=(INPUT)
				type=(if v.secret { "password" } else { "text" })
				autocomplete="off" spellcheck="false" placeholder=(placeholder)
				required[v.kind == "input" && v.required]
				aria-describedby=[v.description.as_ref().map(|_| help_id.as_str())]
				"data-bind"=(bind);
			@if let Some(d) = &v.description {
				p id=(help_id) class="ui-hint" { (d) }
			}
			@if v.secret {
				p class="ui-hint" { "Stored encrypted. It is never shown again." }
			}
		}
	}
}

/// The starting value of every signal the dialog binds.
fn signals(t: &Loaded, vars: &[VariableInfo], targets: &Targets) -> Value {
	let mut s = json!({
		"project": targets.project.map(|p| p.0.to_string()).unwrap_or_default(),
		"environment": targets.environment.map(|e| e.0.to_string()).unwrap_or_default(),
		"server": targets.servers.first().map(|x| x.id.0.to_string()).unwrap_or_default(),
		"name": t.template.name,
		"_error": "",
	});
	for (i, v) in asked(vars).iter().enumerate() {
		// A secret has no default to show; everything else starts at its default.
		s[format!("v{i}")] =
			Value::String(v.default.clone().filter(|_| !v.secret).unwrap_or_default());
	}
	s
}

fn no_target(targets: &Targets) -> Option<Markup> {
	if targets.projects.is_empty() {
		return Some(html! {
			p class="text-sm" { "You need a project to deploy into. Create one first." }
		});
	}
	if targets.servers.is_empty() {
		return Some(html! {
			p class="text-sm" { "No server is active, so nothing can be deployed yet. Check the Servers page." }
		});
	}
	None
}

pub(super) fn dialog(place: Place, l: &Loaded, vars: &[VariableInfo], targets: &Targets) -> Markup {
	let t = &l.template;
	let url = format!("/teams/{}/templates/{}/deploy", place.team, t.id);
	let envs_url = format!("/teams/{}/templates/{}/environments", place.team, t.id);
	let post = format!("evt.preventDefault(); @post({})", js_string(&url));
	let change = format!("@get({})", js_string(&envs_url));
	let blocked = no_target(targets);
	html! {
		dialog id=(DIALOG_ID) class="ui-dialog ui-dialog-wide" closedby="any"
			aria-labelledby="tpl-deploy-title" aria-describedby="tpl-deploy-description"
			"data-signals"=(signals(l, vars, targets).to_string()) {
			div id="tpl-deploy-panel" class="ui-dlg-panel" {
				div class="ui-dialog-head" {
					div {
						h2 id="tpl-deploy-title" class="ui-dialog-title" { "Deploy " (t.name) }
						p id="tpl-deploy-description" class="ui-dialog-desc" {
							@if l.plain_app { "It runs as one application on the server you pick." }
							@else { "It runs as a stack of " (l.services.len()) " services on the server you pick." }
						}
					}
					form method="dialog" {
						button class="ui-icon-btn" aria-label="Close" { (icon("x")) }
					}
				}
				@if let Some(blocked) = &blocked {
					div class="ui-dialog-body" { (blocked) }
					div class="ui-dialog-foot" {
						form method="dialog" { button class=(BUTTON_OUTLINE) autofocus { "Close" } }
					}
				} @else {
					form id="tpl-deploy-form" class="ui-dialog-body" "data-on:submit"=(post) {
						div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
							label class=(LABEL) {
								"Project"
								select class=(SELECT) required autofocus "data-bind:project" "data-on:change"=(change) {
									button { selectedcontent {} }
									@for p in &targets.projects {
										option value=(p.id.0) selected[Some(p.id) == targets.project] { (p.name) }
									}
								}
							}
							(environment_field(&targets.environments, targets.environment))
							label class=(LABEL) {
								"Server"
								select class=(SELECT) required "data-bind:server" {
									button { selectedcontent {} }
									@for x in &targets.servers {
										option value=(x.id.0) { (x.name) }
									}
								}
							}
							label class=(LABEL) {
								"Name"
								input class=(INPUT) type="text" required maxlength="60" autocomplete="off" "data-bind:name";
							}
						}
						@let questions = asked(vars);
						@if !questions.is_empty() {
							fieldset {
								legend { "Variables" }
								@for (i, v) in questions.iter().enumerate() {
									(var_field(i, v))
								}
							}
						}
						(form_error())
					}
					div class="ui-dialog-foot" {
						form method="dialog" { button class=(BUTTON_OUTLINE) { "Cancel" } }
						button type="submit" form="tpl-deploy-form" class=(BUTTON)
							"data-indicator:_busy" "data-attr:disabled"="$_busy" {
							span "data-show"="!$_busy" { "Deploy" }
							span "data-show"="$_busy" style="display: none" { "Deploying" }
						}
					}
				}
			}
		}
	}
}

// --- the environment select follows the project ----------------------------------------

#[derive(Debug, Default, Deserialize)]
pub(super) struct ProjectSignal {
	#[serde(default)]
	project: String,
}

pub(super) async fn environments(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, _)): Path<(TeamId, String)>,
	ReadSignals(sig): ReadSignals<Option<ProjectSignal>>,
) -> Response {
	let Some(project) = sig
		.unwrap_or_default()
		.project
		.parse::<i64>()
		.ok()
		.map(ProjectId)
	else {
		return error::action(&ServerError::Validation("Pick a project.".to_owned()));
	};
	match environments::service::list(&s.db, ctx.user.id, ctx.team.id, project).await {
		Ok(list) => {
			let first = list.first().map(|e| e.id);
			sse::patch(&environment_field(&list, first))
				.and(sse::signals(&json!({
					"environment": first.map(|e| e.0.to_string()).unwrap_or_default(),
				})))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

// --- deploy ------------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct DeploySignals {
	project: String,
	environment: String,
	server: String,
	name: String,
	/// `v0`, `v1`, ...: the answers, in the order of the page's list.
	#[serde(flatten)]
	rest: BTreeMap<String, Value>,
}

fn id(raw: &str, what: &str) -> Result<i64, ServerError> {
	raw.trim()
		.parse()
		.map_err(|_| ServerError::Validation(format!("Pick a {what}.")))
}

/// Where a finished deploy leads: the new application's deployment, or the stack's.
fn target(team: TeamId, project: ProjectId, d: &Deployed) -> String {
	match d.kind {
		Kind::Application => format!(
			"{}/deployments/{}",
			pages::apps::app_url(team, project, ApplicationId(d.id)),
			d.deploy_id
		),
		Kind::Stack => format!(
			"/teams/{team}/projects/{project}/stacks/{}/deployments",
			d.id
		),
	}
}

fn credentials_panel(href: &str, d: &Deployed) -> Markup {
	html! {
		div id="tpl-deploy-panel" class="ui-dlg-panel" {
			div class="ui-dialog-head" {
				div {
					h2 id="tpl-deploy-title" class="ui-dialog-title" { (d.name) " is deploying" }
					p id="tpl-deploy-description" class="ui-dialog-desc" {
						"Use these to sign in. podkit shows them only now and cannot show them again."
					}
				}
				form method="dialog" {
					button class="ui-icon-btn" aria-label="Close" { (icon("x")) }
				}
			}
			dl class="ui-dialog-body" {
				@for (name, value) in &d.credentials {
					div class="ui-field-group" {
						dt class="ui-label" { (name) }
						dd class="flex gap-2" {
							input class=(INPUT) type="text" readonly value=(value) autocomplete="off" spellcheck="false"
								aria-label=(name) "data-on:focus"="el.select()";
							button type="button" class=(BUTTON_OUTLINE)
								"data-on:click"="el.previousElementSibling.select(); document.execCommand('copy')" { "Copy" }
						}
					}
				}
			}
			div class="ui-dialog-foot" {
				(link_button(Variant::Default, Size::Md, href, &html! { "Open " (d.name) }))
			}
		}
	}
}

pub(super) async fn deploy(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, template)): Path<(TeamId, String)>,
	ReadSignals(sig): ReadSignals<DeploySignals>,
) -> Response {
	let result = async {
		let loaded = templates::find(&template).ok_or(ServerError::NotFound)?;
		let project = ProjectId(id(&sig.project, "project")?);
		let server = ServerId(id(&sig.server, "server")?);
		let environment = match sig.environment.trim() {
			"" => None,
			raw => Some(EnvironmentId(id(raw, "environment")?)),
		};
		let vars = templates::variables(&loaded.template);
		let mut inputs = BTreeMap::new();
		for (i, v) in asked(&vars).iter().enumerate() {
			let answer = match sig.rest.get(&format!("v{i}")) {
				Some(Value::String(text)) => text.trim().to_owned(),
				_ => String::new(),
			};
			if !answer.is_empty() {
				inputs.insert(v.name.clone(), answer);
			}
		}
		let name = Some(sig.name.trim().to_owned()).filter(|n| !n.is_empty());
		let deployed = templates::service::deploy(
			&s,
			ctx.user.id,
			ctx.team.id,
			project,
			environment,
			server,
			&template,
			DeployInput {
				name,
				inputs,
				model: None,
			},
		)
		.await?;
		Ok::<_, ServerError>((project, deployed))
	}
	.await;
	match result {
		Ok((project, d)) => {
			let href = target(ctx.team.id, project, &d);
			if d.credentials.is_empty() {
				sse::redirect(&href).into_response()
			} else {
				sse::patch(&credentials_panel(&href, &d))
					.and(sse::signals(&json!({ "_error": "" })))
					.into_response()
			}
		}
		Err(e) => error::action(&e),
	}
}
