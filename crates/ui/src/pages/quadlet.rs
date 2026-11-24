//! Quadlet in the web UI: the read-only "Quadlet" tab of applications, databases, models and
//! stacks, and the "eject" export of a project (its own page) or the whole team (a card in
//! team settings).
//!
//! The tab shows each generated unit in the log style, with its target path and a Copy
//! button. It never shows a secret: the units name their env file and the variables in it,
//! never the values (see [`podkit_core::quadlet`]). The export is a plain form POST, so the browser
//! downloads the archive; the service authorizes and audits it exactly as the API does.

use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Form, Router};
use maud::{Markup, html};
use serde::Deserialize;

use crate::components::{
	BUTTON, CHECKBOX, LABEL, SELECT, card, card_content, card_heading, empty_state, icon,
};
use crate::error;
use crate::extract::TeamCtx;
use crate::layout::{Crumb, app_page};
use crate::sse::js_string;
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::error::ServerError;
use podkit_core::ids::{EnvironmentId, ProjectId, TeamId};
use podkit_core::quadlet::{self, Request, Scope, View};
use podkit_core::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/projects/{project_id}/quadlet",
			get(project_page),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/quadlet/export",
			post(project_export),
		)
		.route(
			"/teams/{team_id}/settings/quadlet/export",
			post(team_export),
		)
}

/// The export page of a project.
pub(crate) fn export_url(team: TeamId, project: ProjectId) -> String {
	format!("/teams/{team}/projects/{project}/quadlet")
}

/// Whether the tab may be shown: the view needs `read` on the resource and on env vars.
pub(crate) fn tab_allowed(ctx: &TeamCtx, resource: Resource) -> bool {
	ctx.perms.can(Action::Read, resource) && ctx.perms.can(Action::Read, Resource::EnvVar)
}

/// Whether the export may be offered: it reads every kind of resource.
fn can_export(ctx: &TeamCtx) -> bool {
	[
		Resource::Project,
		Resource::Application,
		Resource::Database,
		Resource::Model,
		Resource::Stack,
		Resource::EnvVar,
	]
	.into_iter()
	.all(|r| ctx.perms.can(Action::Read, r))
}

/// Whether the export may carry secrets: what revealing them takes.
fn can_export_secrets(ctx: &TeamCtx) -> bool {
	can_export(ctx)
		&& [
			Resource::EnvVar,
			Resource::Database,
			Resource::Model,
			Resource::Stack,
		]
		.into_iter()
		.all(|r| ctx.perms.can(Action::Update, r))
}

// --- the tab ----------------------------------------------------------------------------------

/// The tab's body for a view the service built, or the reason there is none. A resource whose
/// spec cannot be built (a stack whose compose file no longer parses) says why in place; any
/// other failure is the error page.
pub(crate) fn tab(
	ctx: &TeamCtx,
	project: ProjectId,
	view: Result<View, ServerError>,
) -> Result<Markup, Response> {
	match view {
		Ok(view) => Ok(content(ctx, project, &view)),
		Err(ServerError::Conflict(message)) => Ok(card(&card_content(&empty_state(
			"package",
			"No Quadlet units",
			Some(&message),
			None,
		)))),
		Err(e) => Err(error::page(ctx, &e)),
	}
}

/// Copies the lines of `#id` as the unit file's text.
fn copy_script(id: &str) -> String {
	format!(
		"const t = Array.from(document.getElementById({}).children).map(l => {{ const s = l.textContent; return s.trim() === '' ? '' : s }}).join('\\n') + '\\n'; const w = el.querySelector('span'); const done = () => {{ w.textContent = 'Copied'; setTimeout(() => w.textContent = 'Copy', 1600) }}; if (window.isSecureContext && navigator.clipboard) {{ navigator.clipboard.writeText(t).then(done) }} else {{ const a = document.createElement('textarea'); a.value = t; a.setAttribute('readonly', ''); a.style.position = 'fixed'; a.style.opacity = '0'; document.body.append(a); a.select(); document.execCommand('copy'); a.remove(); done() }}",
		js_string(id)
	)
}

/// The level a line is drawn at: comments quiet, section headers marked.
fn level(line: &str) -> &'static str {
	if line.starts_with('#') {
		"DEBUG"
	} else if line.starts_with('[') {
		"STEP"
	} else {
		"INFO"
	}
}

/// One unit file in the log style: its name, where it goes, a Copy button and the text.
fn unit(index: usize, name: &str, path: &str, content: &str) -> Markup {
	let id = format!("quadlet-unit-{index}");
	html! {
		section class="ui-logbox" aria-labelledby={ (id) "-title" } {
			div class="ui-logbar" {
				div class="ui-logbar-main" {
					h2 id={ (id) "-title" } class="ui-logbar-title" {
						"Generated Quadlet unit " span class="ui-logbar-id font-mono" { (name) }
					}
					div class="ui-logbar-end" {
						span class="font-mono text-xs text-muted-foreground" { (path) }
						button type="button" class="ui-button ui-button-outline ui-button-sm" "data-on:click"=(copy_script(&id)) {
							(icon("copy"))
							span { "Copy" }
						}
					}
				}
			}
			div class="ui-log ui-quadlet-log" tabindex="0" role="region" aria-label={ "Contents of " (name) } {
				div id=(id) {
					@for line in content.lines() {
						div class="ui-log-line" data-lv=(level(line)) {
							@if line.is_empty() { " " } @else { (line) }
						}
					}
				}
			}
		}
	}
}

/// The tab: what the units are, the env files they read (names only), then each unit.
fn content(ctx: &TeamCtx, project: ProjectId, view: &View) -> Markup {
	let files = view.files();
	let env_files = view.env_files();
	let mounted = view.mounted();
	let services: Vec<&str> = view
		.parts
		.iter()
		.flat_map(|p| p.units.services.iter().map(String::as_str))
		.collect();
	html! {
		div class="flex flex-col gap-6" {
			(card(&html! {
				(card_heading(
					"Quadlet units",
					Some("What podkit runs, as Podman Quadlet units for systemd. Read-only: podkit does not use them. They are the way out if you ever run these containers without podkit."),
				))
				(card_content(&html! {
					dl class="grid gap-3 text-sm" {
						div {
							dt class="text-muted-foreground" { "Install to" }
							dd class="font-mono" { (proto::quadlet::ROOTLESS_DIR) "/" }
							dd class="text-xs text-muted-foreground" {
								"On server " (view.server) ", as the user that runs Podman; " (proto::quadlet::ROOTFUL_DIR) "/ for rootful Podman."
							}
						}
						div {
							dt class="text-muted-foreground" { "Start" }
							dd class="font-mono" {
								"systemctl --user daemon-reload"
								@for service in &services {
									br; "systemctl --user start " (service)
								}
							}
						}
						@if !env_files.is_empty() {
							div {
								dt class="text-muted-foreground" { "Env files" }
								@for (name, keys) in &env_files {
									dd {
										span class="font-mono" { (name) }
										" holds " (keys.join(", "))
									}
								}
								dd class="text-xs text-muted-foreground" {
									"The values are secrets: they are never shown here, only put in an export that asks for them."
								}
							}
						}
						@if !mounted.is_empty() {
							div {
								dt class="text-muted-foreground" { "Mounted files" }
								@for name in &mounted {
									dd class="font-mono" { (name) }
								}
							}
						}
					}
					@if can_export(ctx) {
						p class="mt-4 text-sm" {
							a class="underline" href=(export_url(ctx.team.id, project)) {
								"Export every unit of the project, with a README"
							}
						}
					}
				}))
			}))
			@for (i, file) in files.iter().enumerate() {
				(unit(i, &file.name, &quadlet::target_path(&file.name), &file.content))
			}
		}
	}
}

// --- the export -------------------------------------------------------------------------------

/// The fields of the export form. A checkbox is absent when unticked.
#[derive(Deserialize, Default)]
struct ExportForm {
	#[serde(default)]
	environment: String,
	#[serde(default)]
	secrets: Option<String>,
}

/// What stops working, for the page; the README of the archive says it at length.
const STOPS: [&str; 5] = [
	"Traefik routing, domains, HTTPS and access rules",
	"Deploys from git, webhooks, previews and rollbacks",
	"Backups and restores of databases and volumes",
	"Cron jobs, uptime checks, metrics, logs and the terminal in podkit",
	"Variable changes: the env files hold the values of the moment you export",
];

fn export_form(action: &str, environments: &[(EnvironmentId, String)], secrets: bool) -> Markup {
	html! {
		form method="post" action=(action) class="flex flex-col gap-4" {
			@if !environments.is_empty() {
				label class=(LABEL) {
					"Environment"
					select class={ (SELECT) " max-w-md" } name="environment" {
						option value="" { "All environments" }
						@for (id, name) in environments {
							option value=(id.0) { (name) }
						}
					}
				}
			}
			@if secrets {
				label class="ui-check text-sm" {
					input type="checkbox" class=(CHECKBOX) name="secrets" value="on";
					"Include the env files, with their secret values. This is recorded in the audit log."
				}
			} @else {
				p class="text-sm text-muted-foreground" {
					"The archive has no env files: putting secrets in it needs update rights on variables, databases, models and stacks."
				}
			}
			div {
				button type="submit" class=(BUTTON) { (icon("download")) "Download .tar.gz" }
			}
		}
	}
}

fn stops_card() -> Markup {
	card(&html! {
		(card_heading("What stops working", Some("Containers started from the units run on their own. podkit no longer does any of this for them:")))
		(card_content(&html! {
			ul class="flex list-disc flex-col gap-1 pl-5 text-sm" {
				@for line in STOPS { li { (line) } }
			}
		}))
	})
}

async fn project_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
) -> Result<Markup, Response> {
	if !can_export(&ctx) {
		return Err(error::page(&ctx, &ServerError::Forbidden));
	}
	let fail = |e: ServerError| error::page(&ctx, &e);
	let project = podkit_core::projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	let environments: Vec<(EnvironmentId, String)> =
		podkit_core::environments::list_by_project(&s.db, project.id)
			.await
			.map_err(|e| fail(e.into()))?
			.into_iter()
			.map(|e| (e.id, e.name))
			.collect();
	let base = export_url(ctx.team.id, project.id);
	let body = html! {
		div class="ui-page-body" {
			header class="ui-page-head" {
				div class="ui-page-title" {
					p class="ui-eyebrow" { (project.name) }
					h1 { "Eject to Quadlet" }
					p class="ui-lede" {
						"Every container of this project as Podman Quadlet units, with a README on installing and starting them. A way to run them with systemd alone, or to read what podkit runs."
					}
				}
			}
			(card(&html! {
				(card_heading("Export units", Some("One folder per server, each with its units and a README.")))
				(card_content(&export_form(&format!("{base}/export"), &environments, can_export_secrets(&ctx))))
			}))
			(stops_card())
		}
	};
	let project_href = format!("/teams/{}/projects/{}", ctx.team.id, project.id);
	let crumbs: Vec<Crumb<'_>> = vec![
		("Projects", Some(format!("/teams/{}/projects", ctx.team.id))),
		(project.name.as_str(), Some(project_href)),
		("Eject to Quadlet", None),
	];
	Ok(app_page(&ctx, "Eject to Quadlet", &crumbs, &body))
}

async fn project_export(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
	Form(form): Form<ExportForm>,
) -> Response {
	let environment = match form.environment.trim() {
		"" => None,
		id => match id.parse::<i64>() {
			Ok(id) => Some(EnvironmentId(id)),
			Err(_) => return error::page(&ctx, &ServerError::EnvironmentNotFound),
		},
	};
	let request = Request {
		scope: Scope::Project {
			project,
			environment,
		},
		secrets: form.secrets.is_some(),
		// A signed-in session may see what RBAC allows.
		can_reveal: true,
	};
	match quadlet::export(&s, ctx.user.id, ctx.team.id, request).await {
		Ok(archive) => quadlet::routes::download(archive),
		Err(e) => error::page(&ctx, &e),
	}
}

async fn team_export(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Form(form): Form<ExportForm>,
) -> Response {
	let request = Request {
		scope: Scope::Team,
		secrets: form.secrets.is_some(),
		can_reveal: true,
	};
	match quadlet::export(&s, ctx.user.id, ctx.team.id, request).await {
		Ok(archive) => quadlet::routes::download(archive),
		Err(e) => error::page(&ctx, &e),
	}
}

/// The team settings card: every project of the team in one archive.
pub(crate) fn team_card(ctx: &TeamCtx) -> Option<Markup> {
	can_export(ctx).then(|| {
		card(&html! {
			(card_heading(
				"Eject to Quadlet",
				Some("Every container of every project as Podman Quadlet units, one folder per server, with a README on installing them and what podkit then stops doing."),
			))
			(card_content(&export_form(
				&format!("/teams/{}/settings/quadlet/export", ctx.team.id),
				&[],
				can_export_secrets(ctx),
			)))
		})
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn lines_are_drawn_by_kind() {
		assert_eq!(level("# a comment"), "DEBUG");
		assert_eq!(level("[Container]"), "STEP");
		assert_eq!(level("Image=x"), "INFO");
	}

	#[test]
	fn a_unit_is_escaped_named_and_copyable() {
		let html = unit(
			0,
			"podkit-app-1.container",
			"~/.config/containers/systemd/podkit-app-1.container",
			"[Container]\nLabel=a=<b>\n\n# note",
		)
		.into_string();
		assert!(html.contains("Generated Quadlet unit"), "{html}");
		assert!(html.contains("podkit-app-1.container"), "{html}");
		assert!(
			html.contains("~/.config/containers/systemd/podkit-app-1.container"),
			"{html}"
		);
		assert!(html.contains("Label=a=&lt;b&gt;"), "{html}");
		assert!(!html.contains("<b>"), "{html}");
		assert!(html.contains("navigator.clipboard"), "{html}");
		assert!(html.contains("data-lv=\"STEP\""), "{html}");
		assert!(html.contains("data-lv=\"DEBUG\""), "{html}");
	}

	#[test]
	fn the_export_form_offers_secrets_only_when_allowed() {
		let with = export_form("/x", &[(EnvironmentId(3), "staging".into())], true).into_string();
		assert!(with.contains("name=\"secrets\""), "{with}");
		assert!(with.contains("value=\"3\""), "{with}");
		assert!(with.contains("method=\"post\""), "{with}");
		let without = export_form("/x", &[], false).into_string();
		assert!(!without.contains("name=\"secrets\""), "{without}");
		assert!(!without.contains("name=\"environment\""), "{without}");
	}
}
