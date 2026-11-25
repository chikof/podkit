//! The Domains tab: the generated address, custom domains, and what each is served over.
//!
//! The generated hostname (`<slug>.<server>.sslip.io`) answers over plain HTTP through the
//! ingress. A custom domain gets a Let's Encrypt certificate when the podkit server has an
//! ACME email, and is served over plain HTTP otherwise: the page says which.

use std::net::IpAddr;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::access;
use super::{AppPath, Tab, app_url, load, page, public_link, saved_slot};
use crate::components::{
	INPUT, LABEL, Size, Tone, Variant, badge, button_class, card, card_content, card_section,
	empty_state, icon,
};
use crate::extract::TeamCtx;
use crate::layout::form_error;
use crate::sse::js_string;
use crate::{error, sse};
use podkit_core::applications::service::ApplicationView;
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::domains::Domain;
use podkit_core::domains::service as domains;
use podkit_core::ids::{ApplicationId, DomainId, ProjectId, TeamId};
use podkit_core::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/domains",
			get(domains_page).post(domain_add),
		)
		.route(
			"/teams/{team_id}/projects/{project_id}/apps/{app_id}/domains/{domain_id}/delete",
			post(domain_remove),
		)
}

const ROW: &str = "ui-card-row";

/// What a custom domain is served over, as a badge: the word is always there.
fn https_badge(https: bool) -> Markup {
	if https {
		badge(Tone::Success, "HTTPS")
	} else {
		badge(Tone::Warning, "HTTP only")
	}
}

fn domain_list(base: &str, domains: &[Domain], can_delete: bool, https: bool) -> Markup {
	html! {
		div id="domain-list" {
			@if domains.is_empty() {
				(empty_state(
					"globe",
					"No custom domains yet.",
					Some("Point a hostname at this application and add it below."),
					None,
				))
			} @else {
				ul class="ui-card-rows" aria-label="Custom domains" {
					@for domain in domains {
						li class=(ROW) {
							div class="flex min-w-0 flex-wrap items-center gap-3" {
								span class="text-sm font-medium break-all" { (domain.hostname) }
								(https_badge(https))
							}
							@if can_delete {
								button type="button" class=(button_class(Variant::Ghost, Size::Icon)) aria-label={ "Remove " (domain.hostname) }
									"data-on:click"={ "@post(" (js_string(&format!("{base}/domains/{}/delete", domain.id))) ")" } {
									(icon("trash-2"))
								}
							}
						}
					}
				}
			}
		}
	}
}

/// What to put in DNS: an address record when the server is reached by IP, else an alias to
/// its name.
fn dns_help(server_host: &str) -> Markup {
	let is_ip = server_host.parse::<IpAddr>().is_ok();
	html! {
		div class="flex flex-col gap-3 text-sm" {
			@if is_ip {
				p { "At your DNS provider, add an " strong { "A" } " record (" strong { "AAAA" } " for an IPv6 address) for the hostname pointing to:" }
			} @else {
				p { "At your DNS provider, add a " strong { "CNAME" } " record for the hostname pointing to:" }
			}
			code class="w-fit rounded-[length:var(--r-xs)] border border-border bg-muted px-2 py-1 font-mono break-all" { (server_host) }
			@if !is_ip {
				p class="text-muted-foreground" { "A bare domain such as example.com can't have a CNAME; use an A record with the server's IP address instead." }
			}
			p class="text-muted-foreground" { "podkit doesn't check DNS. The domain starts to answer after the next deploy, once the record has propagated." }
		}
	}
}

fn https_banner() -> Markup {
	html! {
		div class="rounded-[length:var(--r-md)] border border-warning/30 bg-warning/15 px-4 py-3 text-sm" role="note" {
			strong { "HTTPS is off for custom domains. " }
			"No ACME email is configured on this podkit server, so custom domains are served over plain HTTP. Set "
			code class="font-mono" { "ACME_EMAIL" }
			" in the server's configuration and restart it to get automatic Let's Encrypt certificates."
		}
	}
}

fn default_address(s: &AppState, app: &ApplicationView) -> Markup {
	let link = public_link(s, &app.url);
	html! {
		(card(&html! {
			(card_section("Default address", Some("Generated for every application. It always works, with no DNS to set up."), None))
			(card_content(&html! {
				div class="flex flex-wrap items-center justify-between gap-3" {
					a href=(link) class="inline-flex min-w-0 items-center gap-1 text-sm font-medium hover:underline" target="_blank" rel="noopener noreferrer" {
						span class="break-all" { (link) }
						(icon("external-link"))
					}
					(badge(Tone::Neutral, "HTTP"))
				}
			}))
		}))
	}
}

async fn domains_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	parts: Parts,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Result<Markup, Response> {
	let (project, app) = load(&s, &ctx, project, app).await?;
	let list = domains::list(&s.db, ctx.user.id, ctx.team.id, project.id, app.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let rules = podkit_core::access_rules::service::get(
		&s.db,
		ctx.user.id,
		ctx.team.id,
		project.id,
		app.id,
	)
	.await
	.map_err(|e| error::page(&ctx, &e))?;
	let base = app_url(ctx.team.id, project.id, app.id);
	let can_create = ctx.perms.can(Action::Create, Resource::Domain);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Domain);
	let https = s.config.acme_email.is_some();
	let tailnet =
		podkit_core::tailscale::service::view(&s, ctx.user.id, ctx.team.id, project.id, app.id)
			.await
			.map_err(|e| error::page(&ctx, &e))?;
	let add = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/domains"))
	);
	let mut signals = access::signals(&rules);
	signals.insert("domain_hostname".to_owned(), json!(""));
	signals.insert("_error".to_owned(), json!(""));
	if let Some(from) = super::tailnet::signals(&tailnet).as_object() {
		signals.extend(from.clone());
	}
	let content = html! {
		div class="flex flex-col gap-6" data-signals=(serde_json::Value::Object(signals).to_string()) {
			@if !https { (https_banner()) }
			(saved_slot(&ctx, &base, false))
			(default_address(&s, &app))
			(card(&html! {
				(card_section("Custom domains", Some("Hostnames that route to this application, besides its generated one."), None))
				div class="ui-card-list" {
					div class="ui-card-slot" { (form_error()) }
					(domain_list(&base, &list.domains, can_delete, https))
				}
			}))
			@if can_create {
				(card(&html! {
					(card_section("Add a domain", None, None))
					(card_content(&html! {
						form class="flex flex-col gap-4" "data-on:submit"=(add) {
							label class=(LABEL) {
								"Hostname"
								input class={ (INPUT) " font-mono" } type="text" required placeholder="app.example.com" autocomplete="off" spellcheck="false" autocapitalize="off" "data-bind:domain_hostname";
							}
							div {
								button type="submit" class=(button_class(Variant::Default, Size::Md)) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Add domain" }
							}
						}
					}))
				}))
			}
			(access::section(&ctx, &base, &rules, access::client_ip(&parts)))
			(super::tailnet::tailnet_card(
				&base,
				&tailnet,
				can_create,
				ctx.perms.can(Action::Delete, Resource::Domain),
				&super::tailnet::settings_url(ctx.team.id),
			))
			(card(&html! {
				(card_section("DNS setup", None, None))
				(card_content(&dns_help(&list.server_host)))
			}))
		}
	};
	Ok(page(&s, &ctx, &project, &app, Tab::Domains, &content).await)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DomainSignals {
	domain_hostname: String,
}

async fn domain_add(
	State(s): State<AppState>,
	ctx: TeamCtx,
	path: AppPath,
	ReadSignals(sig): ReadSignals<DomainSignals>,
) -> Response {
	let Path((_, project, app)) = path;
	let result = async {
		domains::create(
			&s.db,
			ctx.user.id,
			ctx.team.id,
			project,
			app,
			sig.domain_hostname.trim(),
		)
		.await?;
		domains::list(&s.db, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(list) => {
			let base = app_url(ctx.team.id, project, app);
			let can_delete = ctx.perms.can(Action::Delete, Resource::Domain);
			let https = s.config.acme_email.is_some();
			sse::patch(&domain_list(&base, &list.domains, can_delete, https))
				.and(sse::patch(&saved_slot(&ctx, &base, true)))
				.and(sse::signals(
					&json!({ "domain_hostname": "", "_error": "" }),
				))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn domain_remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, id)): Path<(TeamId, ProjectId, ApplicationId, DomainId)>,
) -> Response {
	let result = async {
		domains::remove(&s.db, ctx.user.id, ctx.team.id, project, app, id).await?;
		domains::list(&s.db, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(list) => {
			let base = app_url(ctx.team.id, project, app);
			let can_delete = ctx.perms.can(Action::Delete, Resource::Domain);
			let https = s.config.acme_email.is_some();
			sse::patch(&domain_list(&base, &list.domains, can_delete, https))
				.and(sse::patch(&saved_slot(&ctx, &base, true)))
				.and(sse::signals(&json!({ "_error": "" })))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

#[cfg(test)]
mod tests {
	use super::dns_help;

	#[test]
	fn dns_help_asks_for_an_a_record_when_the_server_is_an_ip() {
		let ip = dns_help("203.0.113.7").into_string();
		assert!(ip.contains("<strong>A</strong> record"), "{ip}");
		assert!(ip.contains("203.0.113.7"));
		assert!(!ip.contains("CNAME"));
		let name = dns_help("box.example.com").into_string();
		assert!(name.contains("<strong>CNAME</strong> record"), "{name}");
	}
}
