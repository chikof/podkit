//! The Access part of the Domains tab: whether the application is on the public ingress at
//! all, who may reach it and how fast (IP lists, a rate limit, a body size, the security
//! headers) and its basic-auth users.
//!
//! Traefik does the work: the next deploy writes these settings as middlewares on the
//! container's labels, so a saved change offers to redeploy, like every other setting of an
//! application. Nothing here reads the database: `access_rules::service` hands over an
//! [`AccessView`]. Basic-auth passwords are write-only; the page lists user names only.

use std::net::{IpAddr, SocketAddr};

use axum::Router;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};

use super::{AppPath, app_url, saved_slot};
use crate::access_rules::service::{self as access, AccessView, RulesInput};
use crate::auth::permission::{Action, Resource};
use crate::ids::{ApplicationId, ProjectId, TeamId};
use crate::state::AppState;
use crate::ui::components::{
	BannerKind, INPUT, LABEL, Size, TEXTAREA, Tone, Variant, badge, banner, button_class, card,
	card_content, card_section, empty_state, icon, save_bar, save_bar_saved,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::form_error;
use crate::ui::sse::{self, js_string};
use crate::ui::{error, sse::Events};

const APP: &str = "/teams/{team_id}/projects/{project_id}/apps/{app_id}/access";
const HINT: &str = "ui-hint";

pub fn router() -> Router<AppState> {
	Router::new()
		.route(APP, post(rules_save))
		.route(&format!("{APP}/internal/{{state}}"), post(internal_set))
		.route(&format!("{APP}/users"), post(user_set))
		.route(&format!("{APP}/users/{{name}}/delete"), post(user_remove))
}

/// The signals of the rules form; the save bar watches them.
const FIELDS: &[&str] = &[
	"acc_allow",
	"acc_deny",
	"acc_rate",
	"acc_burst",
	"acc_body",
	"acc_headers",
];

/// The initial value of every signal the section uses.
pub(super) fn signals(view: &AccessView) -> Map<String, Value> {
	let r = &view.rules;
	let num = |n: Option<i32>| n.map_or_else(String::new, |n| n.to_string());
	let Value::Object(map) = json!({
		"acc_allow": r.allow_cidrs.join("\n"),
		"acc_deny": r.deny_cidrs.join("\n"),
		"acc_rate": num(r.rate_average),
		"acc_burst": num(r.rate_burst),
		"acc_body": num(r.max_body_mb),
		"acc_headers": r.security_headers,
		"acc_user": "",
		"acc_pass": "",
	}) else {
		return Map::new();
	};
	map
}

/// Where this request came from, for the "add my IP" button.
pub(super) fn client_ip(parts: &Parts) -> Option<IpAddr> {
	let peer = parts
		.extensions
		.get::<ConnectInfo<SocketAddr>>()
		.map(|c| c.0.ip());
	crate::audit::client_addr(peer, &parts.headers)
}

fn internal_card(base: &str, internal: bool, can_update: bool) -> Markup {
	let (path, label) = if internal {
		("off", "Put on the ingress")
	} else {
		("on", "Make internal only")
	};
	let post = format!(
		"@post({})",
		js_string(&format!("{base}/access/internal/{path}"))
	);
	html! {
		div id="access-internal" {
			(card(&html! {
				(card_section("Public access", Some("An internal-only application has no public route. It answers only to the other resources on its networks."), None))
				(card_content(&html! {
					div class="flex flex-wrap items-center justify-between gap-3" {
						span class="flex items-center gap-2 text-sm" {
							@if internal {
								(badge(Tone::Warning, "Internal only"))
								"No address on the ingress, so nothing outside the server can reach it."
							} @else {
								(badge(Tone::Success, "On the ingress"))
								"Answers on its default address and its domains."
							}
						}
						@if can_update {
							button type="button" class=(button_class(Variant::Outline, Size::Md))
								"data-on:click"=(post) "data-indicator:_busy" "data-attr:disabled"="$_busy" { (label) }
						}
					}
				}))
			}))
		}
	}
}

fn lines_field(label: &str, signal: &str, help_id: &str, help: &str, placeholder: &str) -> Markup {
	html! {
		div class="flex flex-col gap-1" {
			label class=(LABEL) for=(signal) { (label) }
			textarea id=(signal) class=(TEXTAREA) rows="3" placeholder=(placeholder) autocomplete="off" autocapitalize="none"
				autocorrect="off" spellcheck="false" aria-describedby=(help_id) "data-bind"=(signal) {}
			span id=(help_id) class=(HINT) { (help) }
		}
	}
}

fn number_field(label: &str, signal: &str, placeholder: &str) -> Markup {
	html! {
		label class=(LABEL) {
			(label)
			input class=(INPUT) type="text" inputmode="numeric" pattern="[0-9]{1,6}" autocomplete="off"
				placeholder=(placeholder) enterkeyhint="next" "data-bind"=(signal);
		}
	}
}

fn rules_card(base: &str, my_ip: Option<IpAddr>, can_update: bool) -> Markup {
	let save = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/access"))
	);
	let add_ip = my_ip.map(|ip| {
		let ip = js_string(&ip.to_string());
		(
			ip.clone(),
			format!("$acc_allow = ($acc_allow.split(/[\\s,]+/).includes({ip}) ? $acc_allow : ($acc_allow.trim() + '\\n' + {ip}).trim())"),
		)
	});
	html! {
		form class="flex flex-col gap-6" "data-on:submit"=(save) aria-labelledby="access-rules-title" {
			(card(&html! {
				(card_section("Access rules", Some("Traefik checks these before a request reaches the application. They apply to its default address and every domain, from the next deploy."), None))
				(card_content(&html! {
					@if !can_update {
						p class="text-sm text-muted-foreground" { "You can read these rules but not change them." }
					}
					fieldset class="m-0 flex min-w-0 flex-col gap-4 border-0 p-0" disabled[!can_update] {
						legend class="mb-2 text-sm font-medium" id="access-rules-title" { "Who may connect" }
						(lines_field("Allow only these addresses", "acc_allow", "acc-allow-help",
							"One address or range (10.0.0.0/8) per line. Empty allows everyone.", "203.0.113.7\n10.0.0.0/8"))
						@if let Some((_, expr)) = &add_ip {
							div {
								button type="button" class=(button_class(Variant::Outline, Size::Sm)) "data-on:click"=(expr) {
									"Add my IP"
									@if let Some(ip) = my_ip { " (" (ip) ")" }
								}
							}
						}
						(lines_field("Refuse these addresses", "acc_deny", "acc-deny-help",
							"Cut out of the allowed addresses. Traefik has no deny list, so podkit works out the ranges that are left.", "198.51.100.0/24"))
						p class=(HINT) { "Addresses are matched against the connection Traefik sees. Behind another proxy or a NAT that is the proxy's address, not the visitor's." }
					}
					fieldset class="m-0 flex min-w-0 flex-col gap-4 border-0 p-0" disabled[!can_update] {
						legend class="mb-2 text-sm font-medium" { "Limits" }
						div class="grid grid-cols-1 gap-4 sm:grid-cols-3" {
							(number_field("Requests per second", "acc_rate", "No limit"))
							(number_field("Burst", "acc_burst", "1"))
							(number_field("Largest request body (MB)", "acc_body", "No limit"))
						}
						span class=(HINT) { "The rate is per client address. A burst is how many requests above it are let through at once; it needs a rate." }
					}
					div class="ui-check-row" {
						input id="acc-headers" type="checkbox" switch role="switch" class="ui-switch" "data-bind:acc_headers" disabled[!can_update];
						div {
							label for="acc-headers" class="ui-check-title" { "Security headers" }
							small { "HSTS for a year, no framing, no content sniffing, a strict referrer policy. On for new applications; turn it off if the app embeds itself or sets its own." }
						}
					}
				}))
			}))
			@if can_update { (save_bar(FIELDS)) }
		}
	}
}

fn users_card(base: &str, users: &[String], can_update: bool) -> Markup {
	let add = format!(
		"evt.preventDefault(); @post({})",
		js_string(&format!("{base}/access/users"))
	);
	html! {
		div id="access-users" {
			(card(&html! {
				(card_section("Basic auth", Some("Visitors must sign in with one of these users. Passwords are hashed with bcrypt and never shown again."), None))
				div class="ui-card-list" {
					@if users.is_empty() {
						(empty_state("lock", "No login is asked for.", Some("Add a user to put a username and password in front of the application."), None))
					} @else {
						ul class="ui-card-rows" aria-label="Basic auth users" {
							@for name in users {
								li class="ui-card-row" {
									span class="font-mono text-sm break-all" { (name) }
									@if can_update {
										button type="button" class=(button_class(Variant::Ghost, Size::Icon)) aria-label={ "Remove " (name) }
											"data-on:click"={ "@post(" (js_string(&format!("{base}/access/users/{name}/delete"))) ")" } {
											(icon("trash-2"))
										}
									}
								}
							}
						}
					}
				}
				@if can_update {
					(card_content(&html! {
						form class="flex flex-col gap-4" "data-on:submit"=(add) aria-labelledby="access-user-title" {
							h3 id="access-user-title" class="m-0 text-sm font-medium" { "Add a user, or set a new password" }
							div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
								label class=(LABEL) {
									"Username"
									input class={ (INPUT) " font-mono" } type="text" required maxlength="64" autocomplete="off" autocapitalize="none"
										autocorrect="off" spellcheck="false" "data-bind:acc_user";
								}
								label class=(LABEL) {
									"Password"
									input class=(INPUT) type="password" required minlength="8" maxlength="72" autocomplete="new-password"
										"data-bind:acc_pass";
								}
							}
							(form_error())
							div {
								button type="submit" class=(button_class(Variant::Default, Size::Md)) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Save user" }
							}
						}
					}))
				}
			}))
		}
	}
}

/// The whole section, for the Domains tab.
pub(super) fn section(
	ctx: &TeamCtx,
	base: &str,
	view: &AccessView,
	my_ip: Option<IpAddr>,
) -> Markup {
	let can_update = ctx.perms.can(Action::Update, Resource::Domain);
	html! {
		(internal_card(base, view.rules.internal_only, can_update))
		(rules_card(base, my_ip, can_update))
		(users_card(base, &view.users, can_update))
	}
}

/// What the Security tab says about the application's access, with the way to change it.
pub(super) fn security_card(base: &str, view: &AccessView) -> Markup {
	let r = &view.rules;
	let domains = format!("{base}/domains");
	let line = |on: bool, text: &str| {
		html! {
			li class="flex items-center gap-2" {
				(icon(if on { "circle-check" } else { "minus" }))
				span class=(if on { "" } else { "text-muted-foreground" }) { (text) }
			}
		}
	};
	html! {
		(card(&html! {
			(card_section("Access", Some("What Traefik does to requests before they reach this application."), None))
			(card_content(&html! {
				@if r.internal_only {
					(banner(BannerKind::Info, Some("Internal only."), "It has no public route; only the resources on its networks reach it.", None))
				}
				ul class="m-0 flex list-none flex-col gap-2 p-0 text-sm" {
					(line(!r.allow_cidrs.is_empty() || !r.deny_cidrs.is_empty(), "IP allow and deny lists"))
					(line(r.rate_average.is_some(), "Rate limit"))
					(line(!view.users.is_empty(), "Basic auth"))
					(line(r.max_body_mb.is_some(), "Request size limit"))
					(line(r.security_headers, "Security headers"))
				}
				p class="m-0 text-sm" { a class="hover:underline" href=(domains) { "Change access on the Domains tab" } }
			}))
		}))
	}
}

// --- actions -------------------------------------------------------------------------------

fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
	Ok(match Value::deserialize(d)? {
		Value::String(s) => s,
		Value::Number(n) => n.to_string(),
		_ => String::new(),
	})
}

#[derive(Deserialize, Default)]
#[serde(default)]
#[allow(clippy::struct_field_names)] // the names are the page's signals
struct RulesSignals {
	acc_allow: String,
	acc_deny: String,
	#[serde(deserialize_with = "lenient")]
	acc_rate: String,
	#[serde(deserialize_with = "lenient")]
	acc_burst: String,
	#[serde(deserialize_with = "lenient")]
	acc_body: String,
	acc_headers: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct UserSignals {
	acc_user: String,
	acc_pass: String,
}

/// What every action ends with: the "saved, redeploy" banner.
fn saved(ctx: &TeamCtx, base: &str) -> Events {
	sse::patch(&saved_slot(ctx, base, true))
}

async fn rules_save(
	State(s): State<AppState>,
	ctx: TeamCtx,
	path: AppPath,
	ReadSignals(sig): ReadSignals<RulesSignals>,
) -> Response {
	let Path((_, project, app)) = path;
	let input = RulesInput {
		allow: sig.acc_allow,
		deny: sig.acc_deny,
		rate_average: sig.acc_rate,
		rate_burst: sig.acc_burst,
		max_body_mb: sig.acc_body,
		security_headers: sig.acc_headers,
	};
	let result = async {
		access::save(&s.db, ctx.user.id, ctx.team.id, project, app, &input).await?;
		access::get(&s.db, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(view) => {
			let base = app_url(ctx.team.id, project, app);
			let mut fields = signals(&view);
			fields.retain(|k, _| FIELDS.contains(&k.as_str()));
			fields.insert("_error".to_owned(), Value::String(String::new()));
			sse::signals(&Value::Object(fields))
				.and(saved(&ctx, &base))
				.and(sse::toast("Access rules saved"))
				.and(save_bar_saved())
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn internal_set(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, state)): Path<(TeamId, ProjectId, ApplicationId, String)>,
) -> Response {
	let internal = state == "on";
	let result = async {
		access::set_internal(&s.db, ctx.user.id, ctx.team.id, project, app, internal).await?;
		access::get(&s.db, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(view) => {
			let base = app_url(ctx.team.id, project, app);
			let can_update = ctx.perms.can(Action::Update, Resource::Domain);
			sse::patch(&internal_card(&base, view.rules.internal_only, can_update))
				.and(saved(&ctx, &base))
				.and(sse::signals(&json!({ "_error": "" })))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn users_patch(ctx: &TeamCtx, s: &AppState, path: (ProjectId, ApplicationId)) -> Response {
	let (project, app) = path;
	match access::get(&s.db, ctx.user.id, ctx.team.id, project, app).await {
		Ok(view) => {
			let base = app_url(ctx.team.id, project, app);
			let can_update = ctx.perms.can(Action::Update, Resource::Domain);
			sse::patch(&users_card(&base, &view.users, can_update))
				.and(saved(ctx, &base))
				.and(sse::signals(
					&json!({ "acc_user": "", "acc_pass": "", "_error": "" }),
				))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn user_set(
	State(s): State<AppState>,
	ctx: TeamCtx,
	path: AppPath,
	ReadSignals(sig): ReadSignals<UserSignals>,
) -> Response {
	let Path((_, project, app)) = path;
	match access::set_password(
		&s.db,
		ctx.user.id,
		ctx.team.id,
		project,
		app,
		&sig.acc_user,
		&sig.acc_pass,
	)
	.await
	{
		Ok(()) => users_patch(&ctx, &s, (project, app)).await,
		Err(e) => error::action(&e),
	}
}

async fn user_remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, name)): Path<(TeamId, ProjectId, ApplicationId, String)>,
) -> Response {
	match access::remove_user(&s.db, ctx.user.id, ctx.team.id, project, app, &name).await {
		Ok(()) => users_patch(&ctx, &s, (project, app)).await,
		Err(e) => error::action(&e),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_my_ip_button_names_the_address_and_only_exists_when_it_is_known() {
		let ip: IpAddr = "203.0.113.7".parse().unwrap();
		let with = rules_card("/x", Some(ip), true).into_string();
		assert!(with.contains("Add my IP (203.0.113.7)"), "{with}");
		assert!(
			with.contains("type=\"button\""),
			"a helper must not submit the form: {with}"
		);
		let without = rules_card("/x", None, true).into_string();
		assert!(!without.contains("Add my IP"), "{without}");
	}

	#[test]
	fn a_reader_gets_a_disabled_form_and_no_save_bar() {
		let html = rules_card("/x", None, false).into_string();
		assert!(
			html.contains("You can read these rules but not change them."),
			"{html}"
		);
		assert!(!html.contains("ui-save-bar"), "{html}");
		assert!(
			html.contains(
				"<fieldset class=\"m-0 flex min-w-0 flex-col gap-4 border-0 p-0\" disabled"
			),
			"{html}"
		);
	}

	#[test]
	fn the_client_address_is_the_forwarded_one_behind_a_local_proxy_and_never_a_spoofed_header() {
		let parts = |peer: &str, forwarded: Option<&str>| {
			let mut req = axum::http::Request::builder().uri("/");
			if let Some(f) = forwarded {
				req = req.header("x-forwarded-for", f);
			}
			let (mut parts, ()) = req.body(()).unwrap().into_parts();
			parts.extensions.insert(ConnectInfo(
				format!("{peer}:1234").parse::<SocketAddr>().unwrap(),
			));
			parts
		};
		let ip = |p: &Parts| client_ip(p).map(|i| i.to_string());
		// Traefik on this machine: believe it.
		assert_eq!(
			ip(&parts("127.0.0.1", Some("198.51.100.4, 10.0.0.1"))).as_deref(),
			Some("198.51.100.4")
		);
		// A client that talks to podkit directly cannot name itself something else.
		assert_eq!(
			ip(&parts("203.0.113.9", Some("198.51.100.4"))).as_deref(),
			Some("203.0.113.9")
		);
	}
}
