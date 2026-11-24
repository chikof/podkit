//! The Domains tab: one hostname per service. A hostname is routed to the service's port by
//! the next deploy; clearing it stops routing at the next deploy too.
//!
//! Each row binds its own signals (`h<i>` for the hostname, `p<i>` for the port), `i` being
//! the service's place in the name-sorted list, so one page can hold every row's form.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::{Value, json};

use super::frame::{self, Tab};
use super::services::public_link;
use crate::components::{
	BUTTON, INPUT, Size, Variant, button_class, card, card_content, card_heading, empty_state, icon,
};
use crate::extract::TeamCtx;
use crate::sse::{self, js_string};
use crate::{error, status::status_badge};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::ids::{ProjectId, StackId, TeamId};
use podkit_core::stacks::service;
use podkit_core::stacks::{ServiceStatus, StackService};
use podkit_core::state::AppState;

/// The services a domain can be set on, by name.
fn routable(services: &[StackService]) -> Vec<&StackService> {
	let mut list: Vec<&StackService> = services
		.iter()
		.filter(|s| s.status != ServiceStatus::Removing)
		.collect();
	list.sort_by(|a, b| a.name.cmp(&b.name));
	list
}

pub(super) async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
) -> Result<Markup, Response> {
	let l = frame::load(&s, &ctx, project, id).await?;
	let can_update = ctx.perms.can(Action::Update, Resource::Stack);
	let base = l.base(ctx.team.id);
	let list = routable(&l.detail.services);
	let mut signals = json!({ "_error": "" });
	for (i, svc) in list.iter().enumerate() {
		signals[format!("h{i}")] = json!(svc.domain.clone().unwrap_or_default());
		signals[format!("p{i}")] =
			json!(svc.domain_port.map(|p| p.to_string()).unwrap_or_default());
	}
	let content = html! {
		div class="flex flex-col gap-6" data-signals=(signals.to_string()) {
			(card(&html! {
				(card_heading("Domains", Some("Route a hostname to a service. It starts answering after the next deploy.")))
				(card_content(&html! {
					@if list.is_empty() {
						(empty_state("globe", "No services", Some("The compose file defines none."), None))
					} @else {
						ul class="flex flex-col gap-6" {
							@for (i, svc) in list.iter().enumerate() {
								li class="flex flex-col gap-3" data-service=(svc.name) {
									div class="flex flex-wrap items-center gap-3" {
										span class="font-mono text-sm font-medium" { (svc.name) }
										(status_badge(svc.status))
										@if let Some(host) = &svc.domain {
											a class="inline-flex items-center gap-1 text-sm hover:underline" href=(public_link(&s, host)) target="_blank" rel="noopener noreferrer" {
												(host) (icon("external-link"))
											}
										}
									}
									@if can_update {
										form class="grid grid-cols-1 gap-3 sm:grid-cols-[1fr_8rem_auto]"
											"data-on:submit"=(format!("evt.preventDefault(); @post({})", js_string(&format!("{base}/domains/{}", svc.name)))) {
											label class="flex flex-col gap-2 text-sm font-medium" {
												span class="sr-only" { "Hostname for " (svc.name) }
												input class=(INPUT) type="text" autocomplete="off" spellcheck="false"
													placeholder="app.example.com" "data-bind"=(format!("h{i}"));
											}
											label class="flex flex-col gap-2 text-sm font-medium" {
												span class="sr-only" { "Container port for " (svc.name) }
												input class=(INPUT) type="text" inputmode="numeric" placeholder="Port (auto)" "data-bind"=(format!("p{i}"));
											}
											div class="flex items-center gap-2" {
												button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Save" }
												@if svc.domain.is_some() {
													button type="button" class=(button_class(Variant::Outline, Size::Md))
														"data-on:click"=(format!("@post({})", js_string(&format!("{base}/domains/{}/clear", svc.name)))) { "Remove" }
												}
											}
										}
									}
								}
							}
						}
						p class="mt-4 text-sm text-destructive" role="alert" "data-show"="$_error" "data-text"="$_error" style="display: none" {}
					}
				}))
			}))
		}
	};
	Ok(frame::page(&ctx, &l, Tab::Domains, &content))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Signals {
	#[serde(flatten)]
	rest: BTreeMap<String, Value>,
}

fn text(sig: &Signals, key: &str) -> String {
	match sig.rest.get(key) {
		Some(Value::String(t)) => t.trim().to_owned(),
		Some(Value::Number(n)) => n.to_string(),
		_ => String::new(),
	}
}

async fn index_of(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	id: StackId,
	name: &str,
) -> Result<usize, podkit_core::error::ServerError> {
	let detail = service::detail(s, ctx.user.id, ctx.team.id, project, id).await?;
	routable(&detail.services)
		.iter()
		.position(|svc| svc.name == name)
		.ok_or(podkit_core::error::ServerError::NotFound)
}

pub(super) async fn set(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id, name)): Path<(TeamId, ProjectId, StackId, String)>,
	ReadSignals(sig): ReadSignals<Signals>,
) -> Response {
	let result = async {
		let i = index_of(&s, &ctx, project, id, &name).await?;
		let host = text(&sig, &format!("h{i}"));
		let port = match text(&sig, &format!("p{i}")).as_str() {
			"" => None,
			raw => Some(raw.parse::<u16>().map_err(|_| {
				podkit_core::error::ServerError::Validation("The port must be a number.".to_owned())
			})?),
		};
		service::set_domain(
			&s,
			ctx.user.id,
			ctx.team.id,
			project,
			id,
			&name,
			&host,
			port,
		)
		.await
	}
	.await;
	match result {
		Ok(_) => sse::reload().into_response(),
		Err(e) => error::action(&e),
	}
}

pub(super) async fn clear(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id, name)): Path<(TeamId, ProjectId, StackId, String)>,
) -> Response {
	match service::clear_domain(&s, ctx.user.id, ctx.team.id, project, id, &name).await {
		Ok(_) => sse::reload().into_response(),
		Err(e) => error::action(&e),
	}
}
