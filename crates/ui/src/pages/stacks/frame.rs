//! A stack page's frame: header (logo, name, status, Deploy, Stop, menu), the tab row and
//! the actions of the header.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};
use serde_json::json;

use super::{project_url, stack_url};
use crate::components::{
	BUTTON, BUTTON_OUTLINE, BannerKind, LogoSize, MENU_ITEM_DANGER, Priority, Size, TabItem,
	Variant, banner, button_class, dropdown_menu, icon, image_logo, logo, menu_link,
	menu_separator, tabs,
};
use crate::error;
use crate::extract::TeamCtx;
use crate::layout::{Crumb, app_page};
use crate::pages::apps::chrome;
use crate::pages::security;
use crate::sse::{self, js_string};
use crate::status::{Status, poll};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::error::ServerError;
use podkit_core::ids::{ProjectId, StackId, TeamId};
use podkit_core::metrics::service as metrics;
use podkit_core::metrics::{Range, ResourcePoint};
use podkit_core::projects::{self, Project};
use podkit_core::scanning::Subject;
use podkit_core::stacks::service::{self, StackDetail};
use podkit_core::stacks::{ServiceStatus, Stack, StackStatus};
use podkit_core::state::AppState;
use podkit_core::templates;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
	Services,
	Compose,
	Environment,
	Domains,
	Volumes,
	Deployments,
	Logs,
	Terminal,
	Monitoring,
	Networks,
	Security,
	Quadlet,
	Advanced,
}

impl Tab {
	const fn label(self) -> &'static str {
		match self {
			Self::Services => "Services",
			Self::Compose => "Compose",
			Self::Environment => "Environment",
			Self::Domains => "Domains",
			Self::Volumes => "Volumes",
			Self::Deployments => "Deployments",
			Self::Logs => "Logs",
			Self::Terminal => "Terminal",
			Self::Monitoring => "Monitoring",
			Self::Networks => "Networks",
			Self::Security => "Security",
			Self::Quadlet => "Quadlet",
			Self::Advanced => "Advanced",
		}
	}

	const fn path(self) -> &'static str {
		match self {
			Self::Services => "",
			Self::Compose => "/compose",
			Self::Environment => "/env",
			Self::Domains => "/domains",
			Self::Volumes => "/volumes",
			Self::Deployments => "/deployments",
			Self::Logs => "/logs",
			Self::Terminal => "/terminal",
			Self::Monitoring => "/monitoring",
			Self::Networks => "/networks",
			Self::Security => "/security",
			Self::Quadlet => "/quadlet",
			Self::Advanced => "/advanced",
		}
	}

	const ALL: [Self; 13] = [
		Self::Services,
		Self::Compose,
		Self::Environment,
		Self::Domains,
		Self::Volumes,
		Self::Deployments,
		Self::Logs,
		Self::Terminal,
		Self::Monitoring,
		Self::Networks,
		Self::Security,
		Self::Quadlet,
		Self::Advanced,
	];
}

/// A stack with its project, loaded for a page.
pub(super) struct Loaded {
	pub project: Project,
	pub detail: StackDetail,
	/// The newest CPU and memory reading, for the figures under the head.
	pub usage: Option<ResourcePoint>,
	/// The security badge of the head; empty when nothing was scanned or it may not be read.
	pub security: Markup,
}

impl Loaded {
	pub fn stack(&self) -> &Stack {
		&self.detail.stack
	}

	pub fn base(&self, team: TeamId) -> String {
		stack_url(team, self.project.id, self.stack().id.0)
	}
}

pub(super) async fn load(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	id: StackId,
) -> Result<Loaded, Response> {
	let fail = |e: ServerError| error::page(ctx, &e);
	let project = projects::service::find(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(fail)?;
	let detail = service::detail(s, ctx.user.id, ctx.team.id, project.id, id)
		.await
		.map_err(fail)?;
	let usage = metrics::stack(&s.db, ctx.user.id, ctx.team.id, project.id, id, Range::Hour)
		.await
		.ok()
		.and_then(|points| points.last().cloned());
	let security =
		security::header_badge(s, ctx, project.id, Subject::Stack(detail.stack.id)).await;
	Ok(Loaded {
		project,
		detail,
		usage,
		security,
	})
}

/// The id of the template a stack came from (`template:<id>@<version>`), if any.
fn template_of(stack: &Stack) -> Option<&'static templates::Loaded> {
	stack
		.source
		.strip_prefix("template:")
		.and_then(|rest| rest.split('@').next())
		.and_then(templates::find)
}

fn mark(stack: &Stack, size: LogoSize) -> Markup {
	match template_of(stack) {
		Some(l) => image_logo(&podkit_core::logos::template_url(l), size, Priority::Normal),
		None => logo("docker", size),
	}
}

fn source_line(stack: &Stack) -> Markup {
	html! {
		@if let Some(l) = template_of(stack) {
			"From the " (l.template.name) " template"
		} @else {
			"Compose file"
		}
	}
}

pub(super) fn crumbs<'a>(ctx: &TeamCtx, project: &'a Project, last: &'a str) -> Vec<Crumb<'a>> {
	vec![
		("Projects", Some(format!("/teams/{}/projects", ctx.team.id))),
		(
			project.name.as_str(),
			Some(project_url(ctx.team.id, project.id)),
		),
		(last, None),
	]
}

#[allow(
	clippy::too_many_lines,
	reason = "one markup block: the header is read top to bottom like the page it draws"
)]
fn header(ctx: &TeamCtx, l: &Loaded, tab: Tab) -> Markup {
	let stack = l.stack();
	let base = l.base(ctx.team.id);
	let can_update = ctx.perms.can(Action::Update, Resource::Stack);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Stack);
	let status = stack.status;
	let busy = matches!(status, StackStatus::Deploying | StackStatus::Deleting);
	let poll_url = format!(
		"{base}/status{}",
		match tab {
			Tab::Services => "?tab=services",
			Tab::Deployments => "?tab=deployments",
			_ => "",
		}
	);
	let post = |action: &str| format!("@post({})", js_string(&format!("{base}{action}")));
	let services = l
		.detail
		.services
		.iter()
		.filter(|svc| svc.status != ServiceStatus::Removing)
		.count();
	let running = l
		.detail
		.services
		.iter()
		.filter(|svc| svc.status == ServiceStatus::Running)
		.count();
	let live = status == StackStatus::Running;
	let dash = || "\u{2014}".to_owned();
	let figures = [
		("Services", format!("{running} of {services} running")),
		(
			"CPU",
			l.usage
				.as_ref()
				.and_then(|p| p.cpu)
				.filter(|_| live)
				.map_or_else(dash, |c| format!("{c:.1}%")),
		),
		(
			"Memory",
			l.usage
				.as_ref()
				.filter(|_| live)
				.map_or_else(dash, |p| chrome::bytes(p.mem)),
		),
	];
	html! {
		div id="stack-header" class="flex flex-col gap-5" "data-signals"="{_action_error: '', _acting: false}" {
			(poll(&poll_url, status.in_flight()))
			(chrome::head(
				&mark(stack, LogoSize::Lg),
				&html! { (icon("layers")) "Compose stack" },
				&stack.name,
				&html! {
					(chrome::dot(chrome::stack_shape(status), status.label()))
					(l.security)
					span class="inline-flex items-center gap-2" {
						(services) @if services == 1 { " service" } @else { " services" }
						" \u{b7} " (source_line(stack))
					}
				},
				&html! {
					div class="ui-app-actions" {
						@if can_update {
							button type="button" class=(BUTTON) disabled[busy]
								"data-on:click"=(post("/deploy")) "data-indicator:_acting" "data-attr:disabled"="$_acting" {
								(icon("rocket")) @if status == StackStatus::Pending { "Deploy" } @else { "Redeploy" }
							}
							button type="button" class=(BUTTON_OUTLINE) disabled[busy || status != StackStatus::Running]
								"data-on:click"=(post("/stop")) "data-attr:disabled"="$_acting" {
								(icon("square")) "Stop"
							}
						}
						@if can_update || can_delete {
							(dropdown_menu(
								"stack-menu",
								&button_class(Variant::Outline, Size::Icon),
								&format!("More actions for {}", stack.name),
								&icon("ellipsis"),
								&html! {
									(menu_link(&format!("{base}/compose"), &html! { (icon("file-text")) "Compose file" }))
									(menu_link(&format!("{base}/logs"), &html! { (icon("terminal")) "Logs" }))
									(menu_link(&format!("{base}/env"), &html! { (icon("key-round")) "Environment" }))
									@if can_delete {
										(menu_separator())
										a href={ (base) "/advanced" } class=(MENU_ITEM_DANGER) { (icon("trash-2")) "Remove stack" }
									}
								},
							))
						}
					}
				},
			))
			@if stack.status == StackStatus::Failed {
				@if let Some(message) = &stack.status_message {
					div data-status-message { (banner(BannerKind::Error, Some("This stack failed."), message, None)) }
				}
			}
			p class="text-sm text-destructive" role="alert" "data-show"="$_action_error" "data-text"="$_action_error" style="display: none" {}
			(chrome::stats("Live figures", &figures))
		}
	}
}

fn tab_row(ctx: &TeamCtx, l: &Loaded, active: Tab) -> Markup {
	let base = l.base(ctx.team.id);
	let hrefs: Vec<(Tab, String)> = Tab::ALL
		.into_iter()
		.filter(|t| match t {
			Tab::Advanced => ctx.perms.can(Action::Delete, Resource::Stack),
			Tab::Security => ctx.perms.can(Action::Read, Resource::Scan),
			Tab::Quadlet => crate::pages::quadlet::tab_allowed(ctx, Resource::Stack),
			Tab::Volumes => ctx.perms.can(Action::Read, Resource::Backup),
			Tab::Terminal => crate::pages::terminal::allowed(ctx),
			_ => true,
		})
		.map(|t| (t, format!("{base}{}", t.path())))
		.collect();
	let items: Vec<TabItem<'_>> = hrefs
		.iter()
		.map(|(t, href)| TabItem {
			label: t.label(),
			href,
			current: *t == active,
		})
		.collect();
	tabs("Stack sections", &items)
}

/// The page: header, tabs, then the tab's own content.
pub(super) fn page(ctx: &TeamCtx, l: &Loaded, tab: Tab, content: &Markup) -> Markup {
	let stack = l.stack();
	let body = html! {
		div class="flex flex-col gap-6" {
			(header(ctx, l, tab))
			(tab_row(ctx, l, tab))
			(content)
		}
	};
	let title = if tab == Tab::Services {
		stack.name.clone()
	} else {
		format!("{} · {}", tab.label(), stack.name)
	};
	app_page(ctx, &title, &crumbs(ctx, &l.project, &stack.name), &body)
}

/// A header patch for the status poll.
pub(super) fn header_patch(ctx: &TeamCtx, l: &Loaded, tab: Tab) -> sse::Events {
	sse::patch(&header(ctx, l, tab))
}

fn action_error(e: &ServerError) -> Response {
	sse::signals(&json!({ "_action_error": error::message(e) })).into_response()
}

pub(super) async fn deploy(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> Response {
	match service::deploy(&s, ctx.user.id, ctx.team.id, project, id).await {
		Ok((stack, _)) => sse::redirect(&format!(
			"{}/deployments",
			stack_url(ctx.team.id, project, stack.id.0)
		))
		.into_response(),
		Err(e) => action_error(&e),
	}
}

pub(super) async fn stop(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> Response {
	match service::stop(&s, ctx.user.id, ctx.team.id, project, id).await {
		Ok(_) => sse::reload().into_response(),
		Err(e) => action_error(&e),
	}
}

pub(super) fn fail_action(e: &ServerError) -> Response {
	action_error(e)
}
