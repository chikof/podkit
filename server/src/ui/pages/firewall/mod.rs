//! A server's Firewall tab: whether the root helper is there, what is always open, the
//! explicit allows, the preview of a change as a diff and the apply with its confirm window.
//!
//! Every action calls `firewall::service`, which authorizes, scopes and audits; nothing here
//! reads the database or decides who may do what. What the page prints from the helper or from
//! the team's input goes through maud's escaping.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use podkit_firewall::protocol::PreviewInfo;
use serde::Deserialize;
use serde_json::json;

use super::servers::{ServerTab, header as server_header};
use crate::auth::permission::{Action, Resource};
use crate::firewall::service::{self, CONFIRM_SECS, Helper, View};
use crate::firewall::{Rule, RuleInput};
use crate::ids::{FirewallRuleId, ServerId, TeamId};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON_DANGER, BUTTON_OUTLINE, BannerKind, INPUT, LABEL, SELECT, Size, TEXTAREA, Tone, Variant,
	badge, banner, button_class, dialog, dialog_cancel, dialog_open_button, empty_state, icon,
	table,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::sse::{self, js_string};
use crate::ui::{error, pages::servers as server_pages};

const OFF_DIALOG: &str = "firewall-off";
const APPLY_DIALOG: &str = "firewall-apply";
const INSTALL_DIALOG: &str = "firewall-install";
const PREVIEW_ID: &str = "firewall-preview";
const BODY_ID: &str = "firewall-body";

pub fn router() -> Router<AppState> {
	const S: &str = "/teams/{team_id}/servers/{server_id}/firewall";
	Router::new()
		.route(S, get(page))
		.route(&format!("{S}/allow"), post(add))
		.route(&format!("{S}/allow/{{rule_id}}/delete"), post(remove))
		.route(&format!("{S}/ssh"), post(save_ssh))
		.route(&format!("{S}/preview"), post(preview))
		.route(&format!("{S}/apply"), post(apply))
		.route(&format!("{S}/off"), post(turn_off))
		.route(&format!("{S}/install"), post(install))
}

fn base_url(team: TeamId, server: ServerId) -> String {
	format!("/teams/{team}/servers/{server}/firewall")
}

fn post_to(url: &str) -> String {
	format!("@post({})", js_string(url))
}

/// The status line: is the helper there, is the firewall on, is a change waiting.
fn status(view: &View) -> Markup {
	match &view.helper {
		Helper::NotInstalled => html! {
			(badge(Tone::Neutral, "Helper not installed"))
		},
		Helper::Unreachable(_) => html! {
			(badge(Tone::Warning, "Helper not answering"))
		},
		Helper::Ready(s) if s.pending.is_some() => html! {
			(badge(Tone::Progress, "Waiting for confirmation"))
		},
		Helper::Ready(s) if s.enabled => html! { (badge(Tone::Success, "On")) },
		Helper::Ready(_) => html! { (badge(Tone::Neutral, "Off")) },
	}
}

/// The reason a helper gave, as a sentence of its own: the banner puts it after a full stop.
fn sentence(why: &str) -> String {
	if why.is_empty() {
		return String::new();
	}
	let mut chars = why.chars();
	let mut out: String = chars
		.next()
		.map(|c| c.to_uppercase().collect())
		.unwrap_or_default();
	out.push_str(chars.as_str());
	if !out.ends_with(['.', '!', '?']) {
		out.push('.');
	}
	out
}

fn helper_banner(view: &View, base: &str, can_update: bool) -> Markup {
	match &view.helper {
		Helper::Ready(_) => html! {},
		Helper::Unreachable(why) => banner(
			BannerKind::Warning,
			Some("podkit cannot talk to the firewall helper."),
			&sentence(why),
			None,
		),
		Helper::NotInstalled => {
			let action = (can_update && !view.server.is_local).then(|| {
				html! {
					(dialog_open_button(INSTALL_DIALOG, &button_class(Variant::Default, Size::Md), &html! { "Install the helper" }))
				}
			});
			let _ = base;
			let text = if view.server.is_local {
				"The server firewall needs a small program that runs as root. On the podkit host you install it yourself: see docs/firewall.md."
			} else {
				"The server firewall needs a small program that runs as root. Nothing is blocked until you apply rules."
			};
			banner(
				BannerKind::Info,
				Some("The firewall helper is not installed on this server."),
				text,
				action.as_ref(),
			)
		}
	}
}

fn defaults(view: &View) -> Markup {
	let ssh = view.server.ssh_port;
	let limited = &view.settings.ssh_sources;
	html! {
		ul class="ui-fw-always" aria-label="Always open" {
			li { b { "Replies and loopback" } " Connections that are already open, and the server talking to itself." }
			li { b { "Ping" } " ICMP, so the server can be found." }
			li { b { "Web" } " TCP 80 and 443, for the ingress." }
			li {
				b { "SSH" } " TCP " (ssh) ", "
				@if limited.is_empty() { "from anywhere." } @else { "from the ranges below." }
				" SSH sessions open when you apply stay open, podkit's own among them."
			}
		}
	}
}

fn rule_row(base: &str, rule: &Rule, can_update: bool) -> Vec<Markup> {
	let delete = post_to(&format!("{base}/allow/{}/delete", rule.id));
	vec![
		html! { span class="font-mono" { (rule.proto.to_uppercase()) " " (rule.port) } },
		html! {
			@if let Some(s) = &rule.source { code class="font-mono text-xs" { (s) } } @else { "Anywhere" }
		},
		html! { (rule.note) },
		html! {
			@if can_update {
				button type="button" class=(button_class(Variant::Ghost, Size::Icon)) "data-on:click"=(delete)
					"data-indicator:_busy" "data-attr:disabled"="$_busy" {
					(icon("trash-2")) span class="sr-only" { "Remove " (rule.summary()) }
				}
			}
		},
	]
}

fn rules_section(base: &str, view: &View, can_update: bool) -> Markup {
	let rows: Vec<Vec<Markup>> = view
		.rules
		.iter()
		.map(|r| rule_row(base, r, can_update))
		.collect();
	html! {
		div id="firewall-rules" {
			@if view.rules.is_empty() {
				(empty_state(
					"shield",
					"No explicit allows.",
					Some("Only the defaults above are open. Add the ports you publish on purpose, such as a database for a team range."),
					None,
				))
			} @else {
				(table("firewall-rules-table", "Explicit allows", &["Port", "From", "Note", ""], &rows))
			}
		}
	}
}

fn add_form(base: &str) -> Markup {
	let add = format!(
		"evt.preventDefault(); {}",
		post_to(&format!("{base}/allow"))
	);
	html! {
		form id="firewall-add" class="ui-fw-form" "data-on:submit"=(add) {
			div class="ui-field-group" {
				label for="fw-proto" class=(LABEL) { "Protocol" }
				select id="fw-proto" class=(SELECT) "data-bind:fw_proto" {
					option value="tcp" { "TCP" }
					option value="udp" { "UDP" }
				}
			}
			div class="ui-field-group" {
				label for="fw-port" class=(LABEL) { "Port" }
				input id="fw-port" class=(INPUT) type="number" inputmode="numeric" min="1" max="65535" "data-bind:fw_port";
			}
			div class="ui-field-group" {
				label for="fw-source" class=(LABEL) { "From (optional)" }
				input id="fw-source" class=(INPUT) type="text" autocomplete="off" spellcheck="false" placeholder="Anywhere, or 10.0.0.0/24" "data-bind:fw_source";
			}
			div class="ui-field-group" {
				label for="fw-note" class=(LABEL) { "Note" }
				input id="fw-note" class=(INPUT) type="text" maxlength="80" autocomplete="off" "data-bind:fw_note";
			}
			button type="submit" class=(button_class(Variant::Outline, Size::Md))
				"data-indicator:_busy" "data-attr:disabled"="$_busy" { (icon("plus")) "Add allow" }
		}
	}
}

fn ssh_form(base: &str, can_update: bool) -> Markup {
	let save = format!("evt.preventDefault(); {}", post_to(&format!("{base}/ssh")));
	html! {
		form id="firewall-ssh" class="ui-stack" "data-on:submit"=(save) {
			div class="ui-field-group" {
				label for="fw-ssh" class=(LABEL) { "Limit SSH to these ranges" }
				textarea id="fw-ssh" class=(TEXTAREA) rows="2" spellcheck="false" disabled[!can_update]
					aria-describedby="fw-ssh-help" "data-bind:fw_ssh" {}
				small id="fw-ssh-help" class="text-xs text-muted-foreground" {
					"One per line or separated by commas. Empty means anywhere. Sessions open when you apply are kept, so you cannot lock yourself out with a typo."
				}
			}
			@if can_update {
				div { button type="submit" class=(button_class(Variant::Outline, Size::Md))
					"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Save ranges" } }
			}
		}
	}
}

/// What a preview shows: the diff, `+` and `-` marked in text as well as colour.
pub(in crate::ui) fn diff_view(p: &PreviewInfo) -> Markup {
	let changed = p.diff.iter().any(|l| l.op != ' ');
	html! {
		div id=(PREVIEW_ID) class="ui-stack" aria-live="polite" {
			@if !changed {
				p class="ui-lede" { "Nothing would change: the ruleset in force is the one you want." }
			} @else {
				p class="ui-lede" { "Lines starting with + are added, with - removed." }
			}
			@if !p.keep_sources.is_empty() {
				p class="text-sm text-muted-foreground" {
					"SSH stays open for the sessions open now, from " (p.keep_sources.join(", ")) "."
				}
			}
			pre class="ui-fw-diff" tabindex="0" aria-label="Changes to the firewall ruleset" {
				@for l in &p.diff {
					span class={ "ui-fw-line ui-fw-" (match l.op { '+' => "plus", '-' => "minus", _ => "same" }) } {
						(l.op) " " (l.text) "\n"
					}
				}
			}
		}
	}
}

fn apply_dialog(base: &str, has_pending: bool) -> Markup {
	let go = post_to(&format!("{base}/apply"));
	dialog(
		APPLY_DIALOG,
		"Apply the firewall?",
		Some(
			"The helper applies the rules, podkit reaches the server again, and only then are they kept.",
		),
		&html! {
			div class="flex flex-col gap-3" {
				p { "If podkit cannot reach the server within " (CONFIRM_SECS) " seconds of applying, the helper puts the old rules back by itself." }
				@if has_pending { p class="text-sm text-destructive" { "A change is already waiting for confirmation. Try again in a minute." } }
				(form_error())
			}
		},
		&html! {
			(dialog_cancel(&button_class(Variant::Outline, Size::Md), &html! { "Cancel" }))
			button type="button" class=(button_class(Variant::Default, Size::Md)) "data-on:click"=(go)
				"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Apply" }
		},
	)
}

fn off_dialog(base: &str) -> Markup {
	let go = post_to(&format!("{base}/off"));
	dialog(
		OFF_DIALOG,
		"Turn the firewall off?",
		None,
		&html! {
			div class="flex flex-col gap-3" {
				p { "podkit's table is removed from the server. Your rules here stay, so you can apply them again. Published container ports are open to anyone who can reach the server." }
				(form_error())
			}
		},
		&html! {
			(dialog_cancel(&button_class(Variant::Outline, Size::Md), &html! { "Cancel" }))
			button type="button" class=(BUTTON_DANGER) "data-on:click"=(go)
				"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Turn off" }
		},
	)
}

fn install_dialog(base: &str) -> Markup {
	let go = post_to(&format!("{base}/install"));
	dialog(
		INSTALL_DIALOG,
		"Install the firewall helper?",
		Some("A small program that runs as root and owns one nftables table."),
		&html! {
			div class="flex flex-col gap-3" {
				p { "podkit re-runs the server's bootstrap. It uploads " code { "podkit-firewall" } " to " code { "/usr/local/lib/podkit" } ", owned by root, and starts it as a systemd service, using " code { "sudo" } " over the SSH login (or directly when that login is root)." }
				p class="text-sm text-muted-foreground" { "The login needs passwordless sudo. If it fails, the bootstrap log on the Overview page says why. Nothing is blocked until you apply rules." }
				(form_error())
			}
		},
		&html! {
			form method="dialog" { button class=(BUTTON_OUTLINE) autofocus { "Cancel" } }
			button type="button" class=(button_class(Variant::Default, Size::Md)) "data-on:click"=(go)
				"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Install" }
		},
	)
}

fn actions(view: &View, can_update: bool) -> Markup {
	let ready = matches!(view.helper, Helper::Ready(_));
	let on = matches!(&view.helper, Helper::Ready(s) if s.enabled);
	html! {
		@if can_update && ready {
			@if on {
				(dialog_open_button(OFF_DIALOG, &button_class(Variant::Outline, Size::Md), &html! { "Turn off" }))
			}
			(dialog_open_button(APPLY_DIALOG, &button_class(Variant::Default, Size::Md), &html! { (icon("shield")) @if on { "Apply" } @else { "Turn on" } }))
		}
	}
}

/// Everything the page and its actions redraw.
fn body(ctx: &TeamCtx, view: &View) -> Markup {
	let base = base_url(ctx.team.id, view.server.id);
	let can_update = ctx.perms.can(Action::Update, Resource::Server);
	let ready = matches!(view.helper, Helper::Ready(_));
	let pending = matches!(&view.helper, Helper::Ready(s) if s.pending.is_some());
	html! {
		div id=(BODY_ID) class="ui-stack" {
			(helper_banner(view, &base, can_update))
			section class="ui-stack" aria-labelledby="fw-default-title" {
				div class="ui-sec-row" {
					h2 id="fw-default-title" class="ui-sec-h" { "Always open" }
					(status(view))
				}
				(defaults(view))
				(ssh_form(&base, can_update))
			}
			section class="ui-stack" aria-labelledby="fw-rules-title" {
				div {
					h2 id="fw-rules-title" class="ui-sec-h" { "Explicit allows" }
					p class="ui-lede" { "Everything else coming in is dropped, including ports that containers publish: they are filtered before Podman's forwarding." }
				}
				(rules_section(&base, view, can_update))
				@if can_update { (add_form(&base)) }
			}
			@if can_update && ready {
				section class="ui-stack" aria-labelledby="fw-preview-title" {
					div class="ui-sec-row" {
						h2 id="fw-preview-title" class="ui-sec-h" { "Preview" }
						button type="button" class=(button_class(Variant::Outline, Size::Md)) "data-on:click"=(post_to(&format!("{base}/preview")))
							"data-indicator:_busy" "data-attr:disabled"="$_busy" { "Preview changes" }
					}
					div id=(PREVIEW_ID) { p class="ui-lede" { "See what applying would change before you apply it." } }
				}
				(apply_dialog(&base, pending))
				(off_dialog(&base))
			}
			@if can_update && !ready { (install_dialog(&base)) }
		}
	}
}

async fn load(
	s: &AppState,
	ctx: &TeamCtx,
	server: ServerId,
) -> Result<View, crate::error::ServerError> {
	service::get(s, ctx.user.id, ctx.team.id, server).await
}

async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
) -> Result<Markup, Response> {
	let view = load(&s, &ctx, id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let can_update = ctx.perms.can(Action::Update, Resource::Server);
	let mut crumbs = server_pages::crumbs(&ctx);
	crumbs.push((
		view.server.name.as_str(),
		Some(server_pages::detail_url(ctx.team.id, id)),
	));
	crumbs.push(("Firewall", None));
	let signals = json!({
		"fw_proto": "tcp", "fw_port": "", "fw_source": "", "fw_note": "",
		"fw_ssh": view.settings.ssh_sources.join("\n"),
		"_busy": false, "_error": "",
	});
	let content = html! {
		div class="ui-page-body" data-signals=(signals.to_string()) {
			(server_header(ctx.team.id, &view.server, ServerTab::Firewall, &actions(&view, can_update)))
			(body(&ctx, &view))
		}
	};
	Ok(app_page(
		&ctx,
		&format!("{} · Firewall", view.server.name),
		&crumbs,
		&content,
	))
}

/// The page body after a change, the dialogs closed and the add form cleared.
async fn refreshed(s: &AppState, ctx: &TeamCtx, server: ServerId) -> Response {
	let view = match load(s, ctx, server).await {
		Ok(v) => v,
		Err(e) => return error::action(&e),
	};
	sse::patch(&body(ctx, &view))
		.and(sse::signals(&json!({
			"fw_proto": "tcp", "fw_port": "", "fw_source": "", "fw_note": "",
			"fw_ssh": view.settings.ssh_sources.join("\n"), "_error": "",
		})))
		.and(sse::script(
			"document.querySelectorAll('dialog[open]').forEach((d) => d.close())",
		))
		.into_response()
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AddSignals {
	#[serde(rename = "fw_proto")]
	proto: String,
	#[serde(rename = "fw_port")]
	port: String,
	#[serde(rename = "fw_source")]
	source: String,
	#[serde(rename = "fw_note")]
	note: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SshSignals {
	fw_ssh: String,
}

async fn add(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, server)): Path<(TeamId, ServerId)>,
	ReadSignals(sig): ReadSignals<AddSignals>,
) -> Response {
	let input = RuleInput {
		proto: sig.proto,
		port: sig.port,
		source: sig.source,
		note: sig.note,
	};
	match service::add(&s, ctx.user.id, ctx.team.id, server, &input).await {
		Ok(_) => refreshed(&s, &ctx, server).await,
		Err(e) => sse::toast(&error::message(&e)).into_response(),
	}
}

async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, server, rule)): Path<(TeamId, ServerId, FirewallRuleId)>,
) -> Response {
	match service::remove(&s, ctx.user.id, ctx.team.id, server, rule).await {
		Ok(()) => refreshed(&s, &ctx, server).await,
		Err(e) => sse::toast(&error::message(&e)).into_response(),
	}
}

async fn save_ssh(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, server)): Path<(TeamId, ServerId)>,
	ReadSignals(sig): ReadSignals<SshSignals>,
) -> Response {
	match service::save_ssh_sources(&s, ctx.user.id, ctx.team.id, server, &sig.fw_ssh).await {
		Ok(()) => refreshed(&s, &ctx, server).await,
		Err(e) => sse::toast(&error::message(&e)).into_response(),
	}
}

async fn preview(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, server)): Path<(TeamId, ServerId)>,
) -> Response {
	match service::preview(&s, ctx.user.id, ctx.team.id, server).await {
		Ok(p) => sse::patch(&diff_view(&p)).into_response(),
		Err(e) => sse::toast(&error::message(&e)).into_response(),
	}
}

async fn apply(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, server)): Path<(TeamId, ServerId)>,
) -> Response {
	match service::apply(&s, ctx.user.id, ctx.team.id, server).await {
		Ok(_) => sse::reload().into_response(),
		Err(e) => error::action(&e),
	}
}

async fn turn_off(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, server)): Path<(TeamId, ServerId)>,
) -> Response {
	match service::disable(&s, ctx.user.id, ctx.team.id, server).await {
		Ok(()) => sse::reload().into_response(),
		Err(e) => error::action(&e),
	}
}

async fn install(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, server)): Path<(TeamId, ServerId)>,
) -> Response {
	match service::install_helper(&s, ctx.user.id, ctx.team.id, server).await {
		Ok(()) => {
			sse::toast("Installing the helper: follow the bootstrap log on the Overview page.")
				.and(sse::script(
					"document.querySelectorAll('dialog[open]').forEach((d) => d.close())",
				))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

#[cfg(test)]
mod tests {
	use super::sentence;

	#[test]
	fn a_reason_is_shown_as_a_sentence() {
		assert_eq!(
			sentence("the agent did not answer"),
			"The agent did not answer."
		);
		assert_eq!(sentence("Already a sentence."), "Already a sentence.");
		assert_eq!(sentence(""), "");
	}
}
