//! "Create Compose": paste a `docker-compose.yml`, check it, deploy it as a stack.
//!
//! The check (`stacks::service::validate`) is the same one a create runs, so a file that
//! passes here is accepted by Create. Every refused key comes back at once, with its
//! service and path, in the panel under the file.

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::frame::crumbs;
use super::issues::{IssueRow, issues_of, placeholder, problems, valid};
use super::{project_url, stack_url};
use crate::error::ServerError;
use crate::ids::{EnvironmentId, ProjectId, ServerId, TeamId};
use crate::projects;
use crate::servers::{self, ServerStatus};
use crate::stacks::service::{self, CreateInput};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, CHECKBOX, INPUT, LABEL, SELECT, Size, TEXTAREA, Variant, button_class, card,
	card_content, card_header,
};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::sse::{self, js_string};

const PANEL: &str = "compose-check";

#[derive(Debug, Default, Deserialize)]
pub(super) struct NewQuery {
	#[serde(default)]
	environment: Option<i64>,
}

pub(super) async fn new_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	Query(q): Query<NewQuery>,
) -> Result<Markup, Response> {
	let fail = |e: ServerError| error::page(&ctx, &e);
	let project = projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	let servers: Vec<_> = servers::service::list(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(fail)?
		.into_iter()
		.filter(|x| x.status == ServerStatus::Active)
		.collect();
	let base = project_url(ctx.team.id, project.id);
	let post = |action: &str| {
		format!(
			"evt.preventDefault(); @post({})",
			js_string(&format!("{base}/stacks{action}"))
		)
	};
	let signals = json!({
		"name": "",
		"server": servers.first().map(|x| x.id.0.to_string()).unwrap_or_default(),
		"compose": "",
		"envText": "",
		"deploy": true,
		"environment": q.environment.map(|e| e.to_string()).unwrap_or_default(),
		"_error": "",
	});
	let body = html! {
		div class="flex flex-col gap-6" data-signals=(signals.to_string()) {
			(card(&html! {
				(card_header("New compose stack", Some("Paste a docker-compose.yml. podkit runs every service on one private network of the server you pick.")))
				(card_content(&html! {
					form class="flex flex-col gap-5" "data-on:submit"=(post("")) {
						div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
							label class=(LABEL) {
								"Name"
								input class=(INPUT) type="text" required maxlength="60" autocomplete="off" autofocus "data-bind:name";
							}
							label class=(LABEL) {
								"Server"
								select class=(SELECT) required "data-bind:server" {
									button { selectedcontent {} }
									@for x in &servers { option value=(x.id.0) { (x.name) } }
								}
							}
						}
						@if servers.is_empty() {
							p class="text-sm text-destructive" { "No server is active, so nothing can be deployed yet. Check the Servers page." }
						}
						label class=(LABEL) {
							"docker-compose.yml"
							textarea class={ (TEXTAREA) " min-h-80" } required spellcheck="false" autocomplete="off" wrap="off"
								placeholder="services:\n  web:\n    image: nginx:1.27\n    ports:\n      - \"8080:80\"" "data-bind:compose" {}
						}
						(placeholder(PANEL))
						label class=(LABEL) {
							span { "Variables " span class="font-normal text-muted-foreground" { "(optional)" } }
							textarea class=(TEXTAREA) spellcheck="false" autocomplete="off" wrap="off"
								placeholder="DB_PASSWORD=change-me" "data-bind"="envText" {}
							span class="text-sm leading-normal font-normal text-muted-foreground" {
								"One NAME=value per line, for the " code class="font-mono" { "${NAME}" } " the file uses. Values are encrypted and never shown again."
							}
						}
						label class="flex cursor-pointer items-center gap-3 text-sm font-medium" {
							input type="checkbox" class=(CHECKBOX) "data-bind:deploy";
							"Deploy as soon as it is created"
						}
						(form_error())
						div class="flex flex-wrap items-center gap-2" {
							button type="submit" class=(BUTTON) disabled[servers.is_empty()]
								"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Create stack" }
							button type="button" class=(button_class(Variant::Outline, Size::Md))
								"data-on:click"=(post("/validate").replace("evt.preventDefault(); ", ""))
								"data-indicator:_checking" "data-attr:disabled"="$_checking" { "Check file" }
						}
					}
				}))
			}))
		}
	};
	Ok(app_page(
		&ctx,
		"New compose stack",
		&crumbs(&ctx, &project, "New compose stack"),
		&body,
	))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Signals {
	name: String,
	server: String,
	compose: String,
	#[serde(rename = "envText")]
	env_text: String,
	deploy: bool,
	environment: String,
}

/// `NAME=value` per line. Blank lines and `#` comments are skipped; a leading `export ` and
/// one pair of surrounding quotes around the value are dropped.
fn parse_env(text: &str) -> Result<BTreeMap<String, String>, ServerError> {
	let mut env = BTreeMap::new();
	for (n, line) in text.lines().enumerate() {
		let line = line.trim();
		if line.is_empty() || line.starts_with('#') {
			continue;
		}
		let line = line.strip_prefix("export ").unwrap_or(line);
		let Some((key, value)) = line.split_once('=') else {
			return Err(ServerError::Validation(format!(
				"Variables, line {}: write NAME=value.",
				n + 1
			)));
		};
		let value = value.trim();
		let value = ['"', '\'']
			.iter()
			.find_map(|q| {
				value
					.strip_prefix(*q)
					.and_then(|v| v.strip_suffix(*q))
					.filter(|_| value.len() >= 2)
			})
			.unwrap_or(value);
		env.insert(key.trim().to_owned(), value.to_owned());
	}
	Ok(env)
}

/// What a failed check or create shows in the panel; `None` when the failure is not about
/// the file.
fn refusal(e: &ServerError) -> Option<Markup> {
	issues_of(e).map(|(headline, rows): (String, Vec<IssueRow>)| problems(PANEL, &headline, &rows))
}

pub(super) async fn validate(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let result = async {
		let env = parse_env(&sig.env_text)?;
		service::validate(&s, ctx.user.id, ctx.team.id, project, &sig.compose, &env).await
	}
	.await;
	match result {
		Ok((warnings, missing)) => {
			let notes: Vec<String> = warnings
				.iter()
				.map(|w| match &w.service {
					Some(svc) => format!("{svc}: {} ({})", w.message, w.path),
					None => format!("{} ({})", w.message, w.path),
				})
				.collect();
			sse::patch(&valid(PANEL, &notes, &missing))
				.and(sse::error(""))
				.into_response()
		}
		Err(e) => match refusal(&e) {
			Some(panel) => sse::patch(&panel).and(sse::error("")).into_response(),
			None => error::action(&e),
		},
	}
}

pub(super) async fn create(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let result = async {
		let server = sig
			.server
			.trim()
			.parse::<i64>()
			.map(ServerId)
			.map_err(|_| ServerError::Validation("Pick a server.".to_owned()))?;
		let environment = match sig.environment.trim() {
			"" => None,
			raw => Some(EnvironmentId(raw.parse::<i64>().map_err(|_| {
				ServerError::Validation("Unknown environment.".to_owned())
			})?)),
		};
		let (stack, _) = service::create(
			&s,
			ctx.user.id,
			ctx.team.id,
			project,
			CreateInput {
				name: sig.name.clone(),
				compose_yaml: sig.compose.clone(),
				env: parse_env(&sig.env_text)?,
				files: BTreeMap::new(),
				server_id: server,
				environment_id: environment,
				source: None,
				deploy: sig.deploy,
			},
		)
		.await?;
		Ok::<_, ServerError>(stack)
	}
	.await;
	match result {
		Ok(stack) => {
			let base = stack_url(ctx.team.id, project, stack.id.0);
			sse::redirect(&if sig.deploy {
				format!("{base}/deployments")
			} else {
				base
			})
			.into_response()
		}
		Err(e) => match refusal(&e) {
			Some(panel) => sse::patch(&panel)
				.and(sse::error(
					"The file was not accepted. See the problems above.",
				))
				.into_response(),
			None => error::action(&e),
		},
	}
}

#[cfg(test)]
mod unit {
	use super::*;

	#[test]
	fn variables_are_name_equals_value_lines() {
		let env = parse_env(
			"# a comment\n\nDB_PASSWORD=hunter2\nexport NAME = \"quoted value\"\nEMPTY=\nQ='x'\n",
		)
		.unwrap();
		assert_eq!(env["DB_PASSWORD"], "hunter2");
		assert_eq!(env["NAME"], "quoted value");
		assert_eq!(env["EMPTY"], "");
		assert_eq!(env["Q"], "x");
		let err = parse_env("ok=1\nnot a pair").unwrap_err();
		assert!(err.to_string().contains("line 2"), "{err}");
	}
}
