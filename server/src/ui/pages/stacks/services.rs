//! The Services tab: each service of the stack with its image, status, health, domain,
//! Restart and a link to its logs. While the stack is deploying the card re-renders itself
//! every few seconds (the header's poll).

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};
use serde::Deserialize;

use super::frame::{self, Loaded, Tab};
use crate::auth::permission::{Action, Resource};
use crate::ids::{ProjectId, StackId, TeamId};
use crate::stacks::{ServiceStatus, StackService, StackStatus};
use crate::state::AppState;
use crate::ui::components::{
	Size, Variant, button_class, card, card_content, card_heading, empty_state, icon, table,
};
use crate::ui::extract::TeamCtx;
use crate::ui::sse::{self, js_string};
use crate::ui::status::status_badge;

/// `http://host` (and the ingress port when it is not 80).
pub(super) fn public_link(s: &AppState, host: &str) -> String {
	match s.config.ingress_port {
		80 => format!("http://{host}"),
		port => format!("http://{host}:{port}"),
	}
}

fn health(svc: &StackService) -> Markup {
	html! {
		@match svc.health.as_deref() {
			Some(h) if !h.is_empty() => { span class="text-sm" { (h) } }
			_ => { span class="text-sm text-muted-foreground" { "no check" } }
		}
	}
}

fn row(s: &AppState, ctx: &TeamCtx, l: &Loaded, svc: &StackService) -> Vec<Markup> {
	let base = l.base(ctx.team.id);
	let can_update = ctx.perms.can(Action::Update, Resource::Stack);
	let idle = !matches!(l.stack().status, StackStatus::Deleting);
	let restart = format!(
		"@post({})",
		js_string(&format!("{base}/services/{}/restart", svc.name))
	);
	vec![
		html! { span class="font-mono" { (svc.name) } },
		html! { span class="block max-w-56 truncate font-mono text-xs" title=(svc.image) { (svc.image) } },
		html! {
			div class="flex flex-col gap-0.5" {
				(status_badge(svc.status))
				@if svc.status == ServiceStatus::Failed {
					@if let Some(m) = &svc.status_message { span class="max-w-56 text-xs text-destructive" { (m) } }
				}
			}
		},
		health(svc),
		html! {
			@if let Some(host) = &svc.domain {
				a class="inline-flex items-center gap-1 hover:underline" href=(public_link(s, host)) target="_blank" rel="noopener noreferrer" {
					span class="max-w-56 truncate" { (host) } (icon("external-link"))
				}
			} @else {
				span class="text-muted-foreground" { "none" }
			}
		},
		html! {
			div class="flex items-center gap-2" {
				@if svc.container_id.is_some() {
					a class=(button_class(Variant::Ghost, Size::Sm)) href={ (base) "/logs?service=" (svc.name) } { (icon("terminal")) "Logs" }
				}
				@if can_update && idle {
					button type="button" class=(button_class(Variant::Outline, Size::Sm)) "data-on:click"=(restart)
						"data-attr:disabled"="$_acting" "data-indicator:_acting" { (icon("refresh-cw")) "Restart" }
				}
			}
		},
	]
}

pub(super) fn services_card(s: &AppState, ctx: &TeamCtx, l: &Loaded) -> Markup {
	let services: Vec<&StackService> = l
		.detail
		.services
		.iter()
		.filter(|svc| svc.status != ServiceStatus::Removing)
		.collect();
	let rows: Vec<Vec<Markup>> = services.iter().map(|svc| row(s, ctx, l, svc)).collect();
	html! {
		div id="stack-services" {
			(card(&html! {
				(card_heading("Services", Some("One container per service, on this stack's private network.")))
				(card_content(&html! {
					@if services.is_empty() {
						(empty_state("layers", "No services", Some("The compose file defines none."), None))
					} @else {
						(table("stack-services-table", "Services of this stack", &["Service", "Image", "Status", "Health", "Domain", "Actions"], &rows))
					}
					@if !l.detail.missing_vars.is_empty() {
						p class="mt-4 text-sm text-warning" {
							"The compose file uses variables that are not set: "
							@for (i, name) in l.detail.missing_vars.iter().enumerate() {
								@if i > 0 { ", " }
								code class="font-mono text-xs" { (name) }
							}
							". Set them in "
							a class="underline" href={ (l.base(ctx.team.id)) "/env" } { "Environment" } "."
						}
					}
				}))
			}))
		}
	}
}

pub(super) async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> Result<Markup, Response> {
	let l = frame::load(&s, &ctx, project, id).await?;
	let content = services_card(&s, &ctx, &l);
	Ok(frame::page(&ctx, &l, Tab::Services, &content))
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct StatusQuery {
	#[serde(default)]
	tab: String,
}

/// The poll: the header again, and the services card on the Services tab.
pub(super) async fn status(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
	Query(q): Query<StatusQuery>,
) -> Response {
	let l = match frame::load(&s, &ctx, project, id).await {
		Ok(l) => l,
		// A real status, so a poll that lost its stack stops.
		Err(r) => return r,
	};
	let tab = match q.tab.as_str() {
		"services" => Tab::Services,
		"deployments" => Tab::Deployments,
		_ => Tab::Advanced,
	};
	let events = frame::header_patch(&ctx, &l, tab);
	match tab {
		Tab::Services => events
			.and(sse::patch(&services_card(&s, &ctx, &l)))
			.into_response(),
		// Only the status line changes while a job runs; its log streams by itself.
		Tab::Deployments => events
			.and(sse::patch(&super::runs::job_status_line(&l)))
			.into_response(),
		_ => events.into_response(),
	}
}

pub(super) async fn restart(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id, service)): Path<(TeamId, ProjectId, StackId, String)>,
) -> Response {
	match crate::stacks::service::restart_service(
		&s,
		ctx.user.id,
		ctx.team.id,
		project,
		id,
		&service,
	)
	.await
	{
		Ok(_) => sse::redirect(&format!(
			"{}/deployments",
			super::stack_url(ctx.team.id, project, id.0)
		))
		.into_response(),
		Err(e) => frame::fail_action(&e),
	}
}
