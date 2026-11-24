//! Every network of every server on one page: who is on it and whether it can reach the
//! outside. Making, importing and deleting networks stays on a server's own Networks tab,
//! where the agent is; this page is where Infrastructure → Networks leads.

use axum::extract::State;
use axum::response::Response;
use maud::{Markup, html};

use super::crumbs;
use crate::components::{
	Size, Tone, Variant, badge, card, card_content, card_section, empty_state, link_button, table,
};
use crate::error;
use crate::extract::TeamCtx;
use crate::layout::{Crumb, app_page};
use crate::pages::servers::detail_url;
use podkit_core::networks::service::{self as network_service, NetworkView};
use podkit_core::servers::Server;
use podkit_core::servers::service as server_service;
use podkit_core::state::AppState;

fn kind_tag(view: &NetworkView) -> Markup {
	let n = &view.network;
	if n.is_environment() {
		badge(Tone::Progress, "Environment")
	} else if n.imported {
		badge(Tone::Neutral, "Imported")
	} else {
		badge(Tone::Neutral, "User")
	}
}

fn rows(views: &[NetworkView]) -> Vec<Vec<Markup>> {
	views
		.iter()
		.map(|v| {
			let names: Vec<&str> = v.members.iter().map(|m| m.name.as_str()).collect();
			vec![
				html! { span class="font-medium" { (v.network.name) } },
				kind_tag(v),
				html! {
					@if v.network.internal { (badge(Tone::Warning, "No outbound")) } @else { (badge(Tone::Neutral, "Outbound")) }
				},
				html! {
					@if names.is_empty() { span class="text-muted-foreground" { "Nobody yet" } }
					@else { (names.join(", ")) }
				},
			]
		})
		.collect()
}

fn server_card(ctx: &TeamCtx, server: &Server, views: &[NetworkView]) -> Markup {
	let manage = link_button(
		Variant::Outline,
		Size::Sm,
		&format!("{}/networks", detail_url(ctx.team.id, server.id)),
		&html! { "Manage" span class="ui-visually-hidden" { " networks of " (server.name) } },
	);
	html! {
		div id={ "server-" (server.id) } class="ui-infra-netcard" {
			(card(&html! {
				(card_section(&server.name, Some(&format!("{} on this server.", match views.len() { 1 => "1 network".to_owned(), n => format!("{n} networks") })), Some(&manage)))
				(card_content(&html! {
					@if views.is_empty() {
						p class="ui-infra-note" { "None yet. One appears with the first deploy of an environment." }
					} @else {
						(table(
							&format!("networks-of-{}", server.id),
							&format!("Networks on {}", server.name),
							&["Network", "Kind", "Outbound", "Used by"],
							&rows(views),
						))
					}
				}))
			}))
		}
	}
}

pub(super) async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let list = server_service::list(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let mut cards = Vec::new();
	for server in &list {
		let views = network_service::list(&s, ctx.user.id, ctx.team.id, server.id)
			.await
			.map_err(|e| error::page(&ctx, &e))?;
		cards.push(server_card(&ctx, server, &views));
	}
	let body = html! {
		div class="ui-page-body" {
			header class="ui-page-head" {
				div class="ui-page-title" {
					h1 { "Networks" }
					p class="ui-lede" { "Resources on a network reach each other by name. Each environment has its own; add shared ones where two need to talk." }
				}
			}
			@if cards.is_empty() {
				(empty_state("server", "No servers yet", Some("Networks belong to a server. Add a server first."), None))
			} @else {
				div class="ui-infra" { @for c in &cards { (c) } }
			}
		}
	};
	let mut trail: Vec<Crumb<'_>> = crumbs(&ctx);
	trail.push(("Networks", None));
	Ok(app_page(&ctx, "Networks", &trail, &body))
}
