//! `/teams/{t}/servers`: the team's servers as cards (status, hardware, CPU and memory
//! sparklines) and the stepped "Add a server" dialog.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::{detail_url, hardware_parts, host_port, list_url, local_badge};
use crate::auth::permission::{Action, Resource};
use crate::ids::TeamId;
use crate::metrics::{Range, ServerPoint, service as metrics_service};
use crate::servers::service::{self, DEFAULT_PODMAN_SOCKET_PATH, DEFAULT_SSH_PORT, NewServer};
use crate::servers::{Server, host_key_fingerprint};
use crate::state::AppState;
use crate::ui::charts::sparkline;
use crate::ui::components::{
	BUTTON, BUTTON_OUTLINE, Dismiss, INPUT, LABEL, LogoSize, TEXTAREA, dialog, dialog_cancel,
	dialog_open_button, icon, logo,
};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::live::log_view;
use crate::ui::sse::{self, js_string};
use crate::ui::status::{Status, status_badge};

const DIALOG_ID: &str = "add-server";
const FORM_ID: &str = "add-server-form";
const LOG_ID: &str = "bootstrap-log";

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/servers", get(page).post(create))
		.route("/teams/{team_id}/servers/rows", get(rows))
}

fn now() -> i64 {
	time::OffsetDateTime::now_utc().unix_timestamp()
}

/// The last hour of one server, or nothing: a server without readings (or one the sampler
/// cannot reach) is shown without sparklines, never as an error.
async fn recent(s: &AppState, ctx: &TeamCtx, server: &Server) -> Vec<ServerPoint> {
	metrics_service::server(&s.db, ctx.user.id, ctx.team.id, server.id, Range::Hour)
		.await
		.unwrap_or_default()
}

fn percent(used: i64, total: i64) -> Option<f64> {
	(total > 0).then(|| used as f64 * 100.0 / total as f64)
}

/// A labelled sparkline with its latest value in text, so the number is not only a picture.
fn figure(label: &str, values: &[Option<f64>], xs: &[i64], from: i64, to: i64) -> Markup {
	let latest = values.iter().rev().flatten().next();
	html! {
		div class="flex min-w-0 flex-col gap-1" {
			div class="flex items-baseline justify-between text-xs text-muted-foreground" {
				span { (label) }
				span class="font-medium text-foreground" {
					@if let Some(v) = latest { (format!("{v:.0}%")) } @else { "–" }
				}
			}
			(sparkline(xs, values, from, to, Some(100.0)))
		}
	}
}

fn server_card(ctx: &TeamCtx, server: &Server, points: &[ServerPoint], now: i64) -> Markup {
	let fingerprint = server.host_key.as_deref().and_then(host_key_fingerprint);
	let hardware = hardware_parts(server);
	let xs: Vec<i64> = points.iter().map(|p| p.at).collect();
	let cpu: Vec<Option<f64>> = points.iter().map(|p| p.cpu).collect();
	let mem: Vec<Option<f64>> = points
		.iter()
		.map(|p| percent(p.mem_used, p.mem_total))
		.collect();
	// The line starts at the oldest reading, so a young server still fills the card instead
	// of a sliver at the right edge of an hour.
	let hour_ago = now - Range::Hour.duration().whole_seconds();
	let from = xs
		.first()
		.copied()
		.map_or(hour_ago, |t| t.max(hour_ago).min(now - 60));
	html! {
		article class="ui-card ui-link-card flex flex-col gap-4 rounded-xl border border-border bg-card p-5 text-card-foreground shadow-xs" {
			div class="ui-link-card-body" {
				(logo(&server.name, LogoSize::Md))
				div class="flex min-w-0 flex-1 flex-col gap-1" {
					h2 class="truncate text-sm leading-tight font-semibold" {
						a href=(detail_url(ctx.team.id, server.id)) class="ui-link-card-link outline-hidden" { (server.name) }
					}
					p class="truncate text-sm text-muted-foreground" {
						"Linux"
						@if let Some(arch) = &server.arch { " · " (arch) }
						" · " (host_port(server))
					}
					div class="mt-1 flex flex-wrap items-center gap-2" {
						(status_badge(server.status))
						@if server.is_local { (local_badge()) }
						@if let Some(version) = &server.agent_version {
							span class="text-xs text-muted-foreground" { "Agent " (version) }
						}
					}
				}
			}
			@if let Some(message) = &server.status_message {
				p class="text-sm text-destructive" { (message) }
			}
			@if hardware.is_empty() {
				p class="text-xs text-muted-foreground" { "Hardware not reported yet." }
			} @else {
				p class="text-xs text-muted-foreground" { (hardware.join(" · ")) }
			}
			@if let Some(fp) = &fingerprint {
				p class="truncate font-mono text-xs text-muted-foreground" title=(fp) { (fp) }
			}
			@if points.is_empty() {
				p class="border-t border-border pt-3 text-xs text-muted-foreground" { "No readings yet." }
			} @else {
				div class="grid grid-cols-2 gap-4 border-t border-border pt-3" {
					(figure("CPU", &cpu, &xs, from, now))
					(figure("Memory", &mem, &xs, from, now))
				}
			}
		}
	}
}

/// The grid of cards. Re-fetched every few seconds while any server is pending, so its
/// badge turns by itself, and every 30 seconds for the figures.
fn server_list(ctx: &TeamCtx, servers: &[Server], points: &[Vec<ServerPoint>]) -> Markup {
	let pending = servers.iter().any(|s| s.status.in_flight());
	let rows_url = format!("{}/rows", list_url(ctx.team.id));
	let now = now();
	html! {
		div id="server-list" class="ui-card-grid" {
			@if servers.is_empty() {
				p class="text-sm text-muted-foreground" { "No servers yet." }
			}
			@for (server, points) in servers.iter().zip(points) {
				(server_card(ctx, server, points, now))
			}
			@if pending {
				(rows_poll(&rows_url, "3s"))
			}
		}
	}
}

/// A refresh of the card grid that waits while the tab is hidden or the add dialog is
/// open, and sends no signals: the dialog's SSH key signal must never reach a query string.
fn rows_poll(url: &str, every: &str) -> Markup {
	let action = format!(
		"document.hidden || document.querySelector('dialog[open]') || @get({}, {{filterSignals: {{include: /^$/}}}})",
		js_string(url)
	);
	// The duration modifier is part of the attribute name, so a literal per interval.
	html! {
		@if every == "3s" {
			div hidden "data-on-interval__duration.3s"=(action) {}
		} @else {
			div hidden "data-on-interval__duration.30s"=(action) {}
		}
	}
}

async fn load(
	s: &AppState,
	ctx: &TeamCtx,
) -> Result<(Vec<Server>, Vec<Vec<ServerPoint>>), Response> {
	let servers = service::list(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(ctx, &e))?;
	let mut points = Vec::with_capacity(servers.len());
	for server in &servers {
		points.push(recent(s, ctx, server).await);
	}
	Ok((servers, points))
}

fn step_label(n: u8, text: &str) -> Markup {
	html! {
		li class="flex items-center gap-2 text-muted-foreground" "data-class"=(format!("{{'text-foreground': $_step == {n}, 'font-medium': $_step == {n}}}"))
			"data-attr:aria-current"=(format!("$_step == {n} ? 'step' : null")) {
			span class="inline-flex h-5 w-5 items-center justify-center rounded-full border border-border text-xs" { (n) }
			(text)
		}
	}
}

/// The three steps of the dialog: connection, SSH key, bootstrap (the live log).
fn add_dialog(ctx: &TeamCtx) -> Markup {
	let post = format!("@post('{}')", list_url(ctx.team.id));
	// Enter and the buttons all submit the form; `novalidate` lets each step check only its
	// own fields before moving on. (`fieldset.reportValidity()` is always true: a fieldset is
	// barred from constraint validation, so ask its controls, stopping at the first invalid
	// one, which is the one that shows the browser's message.)
	let valid = |id: &str| {
		format!("[...document.getElementById('{id}').elements].every(e => e.reportValidity())")
	};
	let submit = format!(
		"evt.preventDefault(); $_step == 1 ? ({} && ($_step = 2)) : $_step == 2 && {} && {post}",
		valid("add-step-1"),
		valid("add-step-2")
	);
	let body = html! {
		form id=(FORM_ID) novalidate class="flex flex-col gap-4" "data-on:submit"=(submit) "data-indicator:_busy" {
			ol class="flex flex-wrap gap-x-4 gap-y-1 text-xs" aria-label="Steps" {
				(step_label(1, "Connection"))
				(step_label(2, "SSH key"))
				(step_label(3, "Bootstrap"))
			}
			fieldset id="add-step-1" class="m-0 flex min-w-0 flex-col gap-4 border-0 p-0" "data-show"="$_step == 1" {
				legend class="ui-visually-hidden" { "Connection" }
				label class=(LABEL) {
					"Name"
					input class=(INPUT) type="text" required maxlength="60" placeholder="build-box" autofocus "data-bind:name";
				}
				div class="grid gap-4 sm:grid-cols-3" {
					div class="sm:col-span-2" {
						label class=(LABEL) {
							"Hostname or IP"
							input class=(INPUT) type="text" required placeholder="203.0.113.10" autocomplete="off" "data-bind:hostname";
						}
					}
					label class=(LABEL) {
						"SSH port"
						input class=(INPUT) type="number" required min="1" max="65535" "data-bind:ssh_port";
					}
				}
				label class=(LABEL) {
					"SSH user"
					input class=(INPUT) type="text" required placeholder="root" autocomplete="off" "data-bind:ssh_user";
				}
			}
			fieldset id="add-step-2" class="m-0 flex min-w-0 flex-col gap-4 border-0 p-0" "data-show"="$_step == 2" style="display: none" {
				legend class="ui-visually-hidden" { "SSH key" }
				label class=(LABEL) {
					"SSH private key"
					textarea class=(TEXTAREA)
						required rows="6" spellcheck="false" autocomplete="off" autocapitalize="off"
						placeholder="Paste the private key podkit should log in with" "data-bind:ssh_private_key" {}
					span class="text-xs font-normal text-muted-foreground" {
						"Stored encrypted. It is never shown again, here or in the API."
					}
				}
				label class=(LABEL) {
					"Podman socket path"
					input class=(INPUT) type="text" required autocomplete="off" "data-bind:podman_socket_path";
					span class="text-xs font-normal text-muted-foreground" {
						"Where podman listens on that host. " code { "%t" }
						" is systemd's runtime directory: " code { "/run" }
						" for root, " code { "/run/user/<uid>" }
						" for a normal user. The default works for both."
					}
				}
			}
			div id="add-server-result" class="flex flex-col gap-3" "data-show"="$_step == 3" style="display: none" {}
			(form_error())
		}
	};
	let footer = html! {
		div class="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end" "data-show"="$_step == 1" {
			(dialog_cancel(BUTTON_OUTLINE, &html! { "Cancel" }))
			button type="submit" form=(FORM_ID) class=(BUTTON) { "Next" }
		}
		div class="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end" "data-show"="$_step == 2" style="display: none" {
			button type="button" class=(BUTTON_OUTLINE) "data-on:click"="$_step = 1" { "Back" }
			button type="submit" form=(FORM_ID) class=(BUTTON) "data-attr:disabled"="$_busy" { "Add server and bootstrap" }
		}
		div class="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end" "data-show"="$_step == 3" style="display: none" {
			(dialog_cancel(BUTTON_OUTLINE, &html! { "Close" }))
		}
	};
	dialog(
		DIALOG_ID,
		"Add a server",
		Some(
			"podkit connects over SSH, installs its agent and starts it. The server stays pending until that finishes.",
		),
		Dismiss::Esc,
		&body,
		&footer,
	)
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let (servers, points) = load(&s, &ctx).await?;
	let can_create = ctx.perms.can(Action::Create, Resource::Server);
	let signals = format!(
		"{{name: '', hostname: '', ssh_port: {DEFAULT_SSH_PORT}, ssh_user: '', ssh_private_key: '', podman_socket_path: '{DEFAULT_PODMAN_SOCKET_PATH}', _step: 1, _busy: false, _error: ''}}"
	);
	let rows_url = format!("{}/rows", list_url(ctx.team.id));
	// Closing the dialog resets it (and forgets a half-typed key); after a finished add it
	// also refreshes the cards. `close` does not bubble, so the capture phase catches it.
	let on_close = format!(
		"($_step == 3 && @get({}, {{filterSignals: {{include: /^$/}}}})); $_step = 1; $ssh_private_key = ''; $_error = ''",
		js_string(&rows_url)
	);

	let body = html! {
		div class="mx-auto flex w-full max-w-7xl flex-col gap-6" data-signals=(signals) "data-on:close__capture"=(on_close) {
			div class="flex flex-wrap items-start justify-between gap-4" {
				div class="flex flex-col gap-1" {
					h1 class="text-2xl font-semibold" { "Servers" }
					p class="text-sm text-muted-foreground" { "The podman hosts this team deploys to." }
				}
				@if can_create {
					(dialog_open_button(DIALOG_ID, BUTTON, &html! { (icon("plus")) "Add a server" }))
				}
			}
			(server_list(&ctx, &servers, &points))
			(rows_poll(&rows_url, "30s"))
			@if can_create { (add_dialog(&ctx)) }
		}
	};
	Ok(app_page(&ctx, "Servers", &super::crumbs(&ctx), &body))
}

/// The polled re-render of the grid.
async fn rows(State(s): State<AppState>, ctx: TeamCtx) -> Response {
	match service::list(&s.db, ctx.user.id, ctx.team.id).await {
		Ok(servers) => {
			let mut points = Vec::with_capacity(servers.len());
			for server in &servers {
				points.push(recent(&s, &ctx, server).await);
			}
			sse::patch(&server_list(&ctx, &servers, &points)).into_response()
		}
		Err(e) => error::action(&e),
	}
}

/// A number or a numeric string: a number input's signal is one or the other
/// depending on whether the browser has parsed it yet.
#[derive(Deserialize)]
#[serde(untagged)]
enum Port {
	Number(i64),
	Text(String),
}

impl Port {
	fn value(&self) -> Option<i32> {
		match self {
			Self::Number(n) => i32::try_from(*n).ok(),
			Self::Text(t) => t.trim().parse().ok(),
		}
	}
}

#[derive(Deserialize)]
struct CreateSignals {
	name: String,
	hostname: String,
	ssh_port: Port,
	ssh_user: String,
	ssh_private_key: String,
	podman_socket_path: String,
}

/// Step 3 of the dialog: what the server is doing, as the live bootstrap log.
fn bootstrap_step(team: TeamId, server: &Server) -> Markup {
	let url = detail_url(team, server.id);
	html! {
		div id="add-server-result" class="flex flex-col gap-3" "data-show"="$_step == 3" {
			p class="text-sm" {
				"Added " strong { (server.name) } ". podkit is connecting, pinning the host key and installing the agent."
			}
			(log_view(LOG_ID, &format!("{url}/log")))
			p class="text-sm" {
				a class="font-medium underline underline-offset-4" href=(url) { "Open the server" }
			}
		}
	}
}

async fn create(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<CreateSignals>,
) -> Response {
	let Some(ssh_port) = sig.ssh_port.value() else {
		return sse::error("SSH port must be between 1 and 65535.").into_response();
	};
	let new = NewServer {
		name: sig.name,
		hostname: sig.hostname.trim().to_owned(),
		ssh_port,
		ssh_user: sig.ssh_user.trim().to_owned(),
		ssh_private_key: sig.ssh_private_key,
		podman_socket_path: sig.podman_socket_path.trim().to_owned(),
	};
	match service::create(&s, ctx.user.id, ctx.team.id, new).await {
		// The key leaves the signals as soon as the server exists.
		Ok(server) => sse::patch(&bootstrap_step(ctx.team.id, &server))
			.and(sse::signals(
				&json!({ "_step": 3, "_error": "", "ssh_private_key": "" }),
			))
			.into_response(),
		Err(e) => error::action(&e),
	}
}
