//! The Services tab: each service of the stack with its image, status, health, domain,
//! Restart and a link to its logs. While the stack is deploying the card re-renders itself
//! every few seconds (the header's poll).

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};
use serde::Deserialize;

use super::frame::{self, Loaded, Tab};
use crate::components::{
	LogoSize, MENU_ITEM, Size, Variant, button_class, card, card_content, card_section,
	dropdown_menu, empty_state, icon, logo, menu_link, table,
};
use crate::extract::TeamCtx;
use crate::pages::apps::chrome;
use crate::sse::{self, js_string};
use crate::status::Status;
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::ids::{ProjectId, StackId, TeamId};
use podkit_core::stacks::{ServiceStatus, StackService, StackStatus};
use podkit_core::state::AppState;

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
			Some(h) if !h.is_empty() => { span class="text-sm whitespace-nowrap" { (h) } }
			_ => { span class="text-sm whitespace-nowrap text-muted-foreground" { "no check" } }
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
	let has_logs = svc.container_id.is_some();
	let can_restart = can_update && idle;
	vec![
		html! {
			div class="ui-svc" {
				(logo(&svc.name, LogoSize::Sm))
				div class="ui-svc-id" {
					span class="ui-svc-name" { (svc.name) }
					span class="ui-svc-image" title=(svc.image) { (svc.image) }
					@if svc.status == ServiceStatus::Failed {
						@if let Some(m) = &svc.status_message {
							span class="ui-svc-err" { (icon("circle-alert")) (m) }
						}
					}
				}
			}
		},
		html! { (chrome::dot(chrome::service_shape(svc.status), svc.status.label())) },
		health(svc),
		html! {
			@if let Some(host) = &svc.domain {
				a class="ui-svc-link" href=(public_link(s, host)) target="_blank" rel="noopener noreferrer" {
					span { (host) } (icon("external-link"))
				}
			} @else {
				span class="text-muted-foreground" { "none" }
			}
		},
		html! {
			@if has_logs || can_restart {
				(dropdown_menu(
					&format!("svc-{}", svc.name),
					&button_class(Variant::Outline, Size::Icon),
					&format!("Actions for {}", svc.name),
					&icon("ellipsis"),
					&html! {
						@if has_logs {
							(menu_link(&format!("{base}/logs?service={}", svc.name), &html! { (icon("terminal")) "Logs" }))
						}
						@if can_restart {
							button type="button" class=(MENU_ITEM) "data-on:click"=(restart)
								"data-attr:disabled"="$_acting" "data-indicator:_acting" { (icon("refresh-cw")) "Restart" }
						}
					},
				))
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
	let failed = services
		.iter()
		.filter(|svc| svc.status == ServiceStatus::Failed)
		.count();
	let running = services
		.iter()
		.filter(|svc| svc.status == ServiceStatus::Running)
		.count();
	html! {
		div id="stack-services" {
			(card(&html! {
				(card_section(
					"Services",
					Some("One container per service, on this stack's private network."),
					Some(&html! {
						span class="text-xs text-muted-foreground" {
							@if failed > 0 { (failed) " failed \u{b7} " }
							(running) " running"
						}
					}),
				))
				(card_content(&html! {
					@if services.is_empty() {
						(empty_state("layers", "No services", Some("The compose file defines none."), None))
					} @else {
						(table("stack-services-table", "Services of this stack", &["Service", "Status", "Health", "Domain", ""], &rows))
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
	match podkit_core::stacks::service::restart_service(
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
