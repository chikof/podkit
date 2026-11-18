//! A stack page's frame: header (logo, name, status, Deploy, Stop, menu), the tab row and
//! the actions of the header.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};
use serde_json::json;

use super::{project_url, stack_url};
use crate::auth::permission::{Action, Resource};
use crate::error::ServerError;
use crate::ids::{ProjectId, StackId, TeamId};
use crate::projects::{self, Project};
use crate::stacks::service::{self, StackDetail};
use crate::stacks::{Stack, StackStatus};
use crate::state::AppState;
use crate::templates;
use crate::ui::components::{
	BUTTON, BUTTON_OUTLINE, LogoSize, MENU_ITEM_DANGER, Priority, Size, TabItem, Variant,
	button_class, dropdown_menu, icon, image_logo, logo, menu_link, menu_separator, tabs,
};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{Crumb, app_page};
use crate::ui::sse::{self, js_string};
use crate::ui::status::{Status, poll, status_badge};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
	Services,
	Compose,
	Environment,
	Domains,
	Deployments,
	Logs,
	Advanced,
}

impl Tab {
	const fn label(self) -> &'static str {
		match self {
			Self::Services => "Services",
			Self::Compose => "Compose",
			Self::Environment => "Environment",
			Self::Domains => "Domains",
			Self::Deployments => "Deployments",
			Self::Logs => "Logs",
			Self::Advanced => "Advanced",
		}
	}

	const fn path(self) -> &'static str {
		match self {
			Self::Services => "",
			Self::Compose => "/compose",
			Self::Environment => "/env",
			Self::Domains => "/domains",
			Self::Deployments => "/deployments",
			Self::Logs => "/logs",
			Self::Advanced => "/advanced",
		}
	}

	const ALL: [Self; 7] = [
		Self::Services,
		Self::Compose,
		Self::Environment,
		Self::Domains,
		Self::Deployments,
		Self::Logs,
		Self::Advanced,
	];
}

/// A stack with its project, loaded for a page.
pub(super) struct Loaded {
	pub project: Project,
	pub detail: StackDetail,
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
	Ok(Loaded { project, detail })
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
		Some(l) => image_logo(&crate::logos::template_url(l), size, Priority::Normal),
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
	html! {
		div id="stack-header" class="flex flex-col gap-3" "data-signals"="{_action_error: '', _acting: false}" {
			(poll(&poll_url, status.in_flight()))
			div class="flex flex-wrap items-start justify-between gap-4" {
				div class="flex min-w-0 items-center gap-4" {
					(mark(stack, LogoSize::Lg))
					div class="flex min-w-0 flex-col gap-1" {
						h1 class="truncate text-2xl font-semibold tracking-tight" { (stack.name) }
						div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-muted-foreground" {
							(status_badge(status))
							span { (source_line(stack)) }
						}
					}
				}
				@if can_update || can_delete {
					div class="flex flex-wrap items-center gap-2" {
						@if can_update {
							button type="button" class=(BUTTON) disabled[busy]
								"data-on:click"=(post("/deploy")) "data-indicator:_acting" "data-attr:disabled"="$_acting" {
								(icon("rocket")) "Deploy"
							}
							button type="button" class=(BUTTON_OUTLINE) disabled[busy || status != StackStatus::Running]
								"data-on:click"=(post("/stop")) "data-attr:disabled"="$_acting" {
								(icon("square")) "Stop"
							}
						}
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
			}
			@if stack.status == StackStatus::Failed {
				@if let Some(message) = &stack.status_message {
					p class="text-sm text-destructive" role="alert" data-status-message { (message) }
				}
			}
			p class="text-sm text-destructive" role="alert" "data-show"="$_action_error" "data-text"="$_action_error" style="display: none" {}
		}
	}
}

fn tab_row(ctx: &TeamCtx, l: &Loaded, active: Tab) -> Markup {
	let base = l.base(ctx.team.id);
	let hrefs: Vec<(Tab, String)> = Tab::ALL
		.into_iter()
		.filter(|t| match t {
			Tab::Advanced => ctx.perms.can(Action::Delete, Resource::Stack),
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
