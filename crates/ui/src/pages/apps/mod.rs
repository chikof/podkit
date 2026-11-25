//! Application pages: an Operate surface, the resource page of one application.
//!
//! A header (logo, name, type, status, public URL, Deploy / Restart / Stop and a menu with
//! Rollback, Promote and Delete) over a row of tabs, each a page of its own: General, Environment,
//! Domains, Deployments, Logs, Volumes, Models, Webhooks and Advanced. The create flows
//! (a Git repository or a Docker image) are one page with sections, `create`.
//!
//! Credentials are write-only. Nothing here puts a stored deploy key, token, registry
//! password, environment variable value or the webhook secret (after the create response)
//! into HTML or a signal.

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderMap, header as http_header};
use axum::response::Response;
use datastar::consts::ElementPatchMode;
use maud::{Markup, html};

use crate::components::{Size, TabItem, Tone, Variant, button_class, tabs, toast};
use crate::error;
use crate::extract::TeamCtx;
use crate::layout::{Crumb, app_page};
use crate::sse;
use crate::sse::js_string;
use podkit_core::applications::SourceKind;
use podkit_core::applications::service::{self, ApplicationView};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::deployments::service as deployments;
use podkit_core::error::ServerError;
use podkit_core::ids::{ApplicationId, ProjectId, TeamId};
use podkit_core::projects::{self, Project};
use podkit_core::state::AppState;

mod access;
mod advanced;
mod auto_update_card;
mod build_tab;
pub mod chrome;
mod create;
mod cron_tab;
mod deployments_tab;
mod domains;
mod env;
mod form;
mod general;
mod git_tab;
mod header;
mod health;
mod logs;
mod mcp_tab;
mod models_tab;
mod monitoring;
mod move_app;
mod networks_tab;
mod previews_card;
mod promote;
mod provider_source;
mod quadlet_tab;
mod replica_list;
mod replicas;
mod security;
mod tailnet;
mod terminal_tab;
mod volumes;
mod webhooks;

#[cfg(test)]
mod tests;

pub fn router() -> Router<AppState> {
	Router::new()
		.merge(create::router())
		.merge(general::router())
		.merge(build_tab::router())
		.merge(header::router())
		.merge(deployments_tab::router())
		.merge(promote::router())
		.merge(move_app::router())
		.merge(logs::router())
		.merge(terminal_tab::router())
		.merge(env::router())
		.merge(domains::router())
		.merge(access::router())
		.merge(tailnet::router())
		.merge(volumes::router())
		.merge(models_tab::router())
		.merge(mcp_tab::router())
		.merge(networks_tab::router())
		.merge(cron_tab::router())
		.merge(monitoring::router())
		.merge(security::router())
		.merge(git_tab::router())
		.merge(webhooks::router())
		.merge(quadlet_tab::router())
		.merge(auto_update_card::router())
		.merge(advanced::router())
}

pub fn project_url(team: TeamId, project: ProjectId) -> String {
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

/// The site the browser used, e.g. `https://podkit.example`: the proxy's forwarded host and
/// protocol when it sets them, else `Host` over plain HTTP.
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
		.or_else(|| first(http_header::HOST.as_str()))
		.unwrap_or("localhost");
	format!("{proto}://{host}")
}

fn crumbs<'a>(ctx: &TeamCtx, project: &'a Project, last: &'a str) -> Vec<Crumb<'a>> {
	vec![
		("Projects", Some(format!("/teams/{}/projects", ctx.team.id))),
		(
			project.name.as_str(),
			Some(project_url(ctx.team.id, project.id)),
		),
		(last, None),
	]
}

// --- the tabs ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
	Deployments,
	Logs,
	Terminal,
	Env,
	Domains,
	Volumes,
	Models,
	Mcp,
	Networks,
	Cron,
	Git,
	Webhooks,
	Monitoring,
	Security,
	Build,
	Quadlet,
	Settings,
	Advanced,
}

impl Tab {
	const ALL: [Self; 18] = [
		Self::Deployments,
		Self::Logs,
		Self::Terminal,
		Self::Env,
		Self::Domains,
		Self::Volumes,
		Self::Models,
		Self::Mcp,
		Self::Networks,
		Self::Cron,
		Self::Git,
		Self::Webhooks,
		Self::Monitoring,
		Self::Security,
		Self::Build,
		Self::Quadlet,
		Self::Settings,
		Self::Advanced,
	];

	const fn label(self) -> &'static str {
		match self {
			Self::Deployments => "Deployments",
			Self::Logs => "Logs",
			Self::Terminal => "Terminal",
			Self::Env => "Environment",
			Self::Domains => "Domains",
			Self::Volumes => "Volumes",
			Self::Models => "Models",
			Self::Mcp => "MCP",
			Self::Networks => "Networks",
			Self::Cron => "Cron",
			Self::Git => "Git",
			Self::Webhooks => "Webhooks",
			Self::Monitoring => "Monitoring",
			Self::Security => "Security",
			Self::Build => "Build",
			Self::Quadlet => "Quadlet",
			Self::Settings => "Settings",
			Self::Advanced => "Advanced",
		}
	}

	/// The path under the application's page. The application's own page is its
	/// deployments, with the newest one's log beside them.
	const fn path(self) -> &'static str {
		match self {
			Self::Deployments => "",
			Self::Logs => "/logs",
			Self::Terminal => "/terminal",
			Self::Env => "/env",
			Self::Domains => "/domains",
			Self::Volumes => "/volumes",
			Self::Models => "/models",
			Self::Mcp => "/mcp",
			Self::Networks => "/networks",
			Self::Cron => "/cron",
			Self::Git => "/git",
			Self::Webhooks => "/webhooks",
			Self::Monitoring => "/monitoring",
			Self::Security => "/security",
			Self::Build => "/build",
			Self::Quadlet => "/quadlet",
			Self::Settings => "/settings",
			Self::Advanced => "/advanced",
		}
	}

	/// What a person must be allowed for the tab to be shown (and its page to open).
	fn allowed(self, ctx: &TeamCtx) -> bool {
		let can = |action, resource| ctx.perms.can(action, resource);
		match self {
			Self::Settings | Self::Logs => true,
			Self::Terminal => can(Action::Create, Resource::Terminal),
			Self::Env => can(Action::Read, Resource::EnvVar),
			Self::Domains => can(Action::Read, Resource::Domain),
			Self::Deployments => can(Action::Read, Resource::Deployment),
			Self::Volumes => can(Action::Read, Resource::Volume),
			Self::Models => can(Action::Read, Resource::Model),
			Self::Networks | Self::Monitoring | Self::Mcp | Self::Git | Self::Build => {
				can(Action::Read, Resource::Application)
			}
			Self::Security => can(Action::Read, Resource::Scan),
			Self::Cron => can(Action::Read, Resource::Cron),
			Self::Quadlet => crate::pages::quadlet::tab_allowed(ctx, Resource::Application),
			Self::Webhooks => can(Action::Update, Resource::Application),
			Self::Advanced => {
				can(Action::Update, Resource::Application)
					|| can(Action::Delete, Resource::Application)
			}
		}
	}
}

fn tab_row(ctx: &TeamCtx, app: &ApplicationView, active: Tab) -> Markup {
	let base = app_url(ctx.team.id, app.project_id, app.id);
	let links: Vec<(Tab, String)> = Tab::ALL
		.into_iter()
		.filter(|t| t.allowed(ctx))
		.filter(|t| !matches!(t, Tab::Git | Tab::Build) || app.source_kind == SourceKind::Git)
		.map(|t| (t, format!("{base}{}", t.path())))
		.collect();
	let items: Vec<TabItem<'_>> = links
		.iter()
		.map(|(tab, href)| TabItem {
			label: tab.label(),
			href,
			current: *tab == active,
		})
		.collect();
	tabs("Application sections", &items)
}

// --- loading and the frame -----------------------------------------------------------------

async fn load(
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

/// What the header offers depends on the newest deployments (Rollback needs one).
async fn recent_deployments(
	s: &AppState,
	ctx: &TeamCtx,
	app: &ApplicationView,
) -> Vec<deployments::DeploymentView> {
	if !ctx.perms.can(Action::Read, Resource::Deployment) {
		return Vec::new();
	}
	deployments::list(&s.db, ctx.user.id, ctx.team.id, app.project_id, app.id)
		.await
		.unwrap_or_default()
}

/// The page frame of every tab: header, tab row, `content`, and the dialogs the header's
/// menu opens. The dialogs sit outside the header, which polling re-renders.
async fn page(
	s: &AppState,
	ctx: &TeamCtx,
	project: &Project,
	app: &ApplicationView,
	active: Tab,
	content: &Markup,
) -> Markup {
	let rows = recent_deployments(s, ctx, app).await;
	// The log of this application, on its landing tab, for people who may read it.
	let activity = if active == Tab::Deployments {
		crate::pages::settings::activity(s, ctx, "application", app.id.0).await
	} else {
		None
	};
	let body = html! {
		div class="flex flex-col gap-6" data-signals="{_action_error: '', _acting: false, _rb: '', _close: '', confirm: '', _delete_error: ''}" {
			(header::header(s, ctx, app, &rows).await)
			(tab_row(ctx, app, active))
			(content)
			@if let Some((events, all)) = &activity {
				(crate::components::activity_card(ctx.team.id, events, all))
			}
			(header::dialogs(ctx, app))
		}
	};
	let title = match active {
		Tab::Deployments => app.name.clone(),
		tab => format!("{} · {}", tab.label(), app.name),
	};
	app_page(ctx, &title, &crumbs(ctx, project, &app.name), &body)
}

/// A tab's page that needs `tab.allowed` before anything is read.
fn forbid_unless_allowed(ctx: &TeamCtx, tab: Tab) -> Result<(), Response> {
	if tab.allowed(ctx) {
		Ok(())
	} else {
		Err(error::page(ctx, &ServerError::Forbidden))
	}
}

/// The standard extraction for tab pages: `/teams/{t}/projects/{p}/apps/{a}`.
type AppPath = Path<(TeamId, ProjectId, ApplicationId)>;

// --- feedback shared by the tabs ----------------------------------------------------------------

/// What a change to the application's configuration says: nothing happens until the next
/// deploy.
const SAVED: &str = "Saved. Changes apply on the next deploy.";

/// The slot the "Saved" banner is patched into: empty until a change lands, then the message
/// and, for someone who may deploy, a button that does it now.
fn saved_slot(ctx: &TeamCtx, base: &str, saved: bool) -> Markup {
	let can_deploy = ctx.perms.can(Action::Create, Resource::Deployment);
	let deploy = format!("@post({})", js_string(&format!("{base}/deploy")));
	html! {
		div id="saved" {
			@if saved {
				div class="flex flex-wrap items-center justify-between gap-3 rounded-[length:var(--r-md)] border border-success/30 bg-success/15 px-4 py-3 text-sm" role="status" {
					span { (SAVED) }
					@if can_deploy {
						button type="button" class=(button_class(Variant::Outline, Size::Sm)) "data-on:click"=(deploy) { "Redeploy now" }
					}
				}
			}
		}
	}
}

/// A success toast, appended to the page's toast region and gone after a few seconds.
fn toast_ok(message: &str) -> sse::Events {
	sse::patch_into(
		"#toasts",
		ElementPatchMode::Append,
		&html! {
			div class="contents" "data-init"="setTimeout(() => el.remove(), 5000)" {
				(toast(Tone::Success, message, None))
			}
		},
	)
}

/// Put inside a dialog: it closes that dialog when the server sets the `_close` signal to the
/// dialog's id (`close_dialog`), so an action can close the form it came from.
fn closer(id: &str) -> Markup {
	let effect = format!(
		"if ($_close === {}) {{ el.closest('dialog').close(); $_close = '' }}",
		js_string(id)
	);
	html! { span hidden "data-effect"=(effect) {} }
}

fn close_dialog(id: &str) -> sse::Events {
	sse::signals(&serde_json::json!({ "_close": id }))
}
