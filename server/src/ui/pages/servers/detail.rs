//! A server's Overview: status and facts (with the hardware the agent reported), the live
//! bootstrap log, re-run, host key reset (behind a confirm) and delete (behind a confirm).

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::{
	ServerTab, crumbs, detail_url, host_port, human_mb, list_url, local_badge, server_tabs,
};
use crate::auth::permission::{Action, Resource};
use crate::error::ServerError;
use crate::ids::{ServerId, TeamId};
use crate::jobs::{Job, JobStatus};
use crate::servers::service;
use crate::servers::{Server, host_key_fingerprint};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON_DANGER, BUTTON_OUTLINE, INPUT, LogoSize, dialog, dialog_cancel, dialog_open_button,
	icon, logo,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::live::{job_log, log_view};
use crate::ui::pages::security;
use crate::ui::status::{Status, poll, status_badge};
use crate::ui::{error, sse};

const LOG_ID: &str = "bootstrap-log";
const RESET_DIALOG: &str = "reset-host-key";
const DELETE_DIALOG: &str = "delete-server";

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/servers/{server_id}", get(detail))
		.route("/teams/{team_id}/servers/{server_id}/status", get(status))
		.route(
			"/teams/{team_id}/servers/{server_id}/log",
			get(bootstrap_log),
		)
		.route(
			"/teams/{team_id}/servers/{server_id}/bootstrap",
			post(bootstrap),
		)
		.route("/teams/{team_id}/servers/{server_id}/delete", post(remove))
}

fn job_label(status: JobStatus) -> &'static str {
	match status {
		JobStatus::Queued => "queued",
		JobStatus::Running => "running",
		JobStatus::Succeeded => "finished",
		JobStatus::Failed => "failed",
		JobStatus::Cancelled => "cancelled",
	}
}

fn job_in_flight(job: Option<&Job>) -> bool {
	job.is_some_and(|j| matches!(j.status, JobStatus::Queued | JobStatus::Running))
}

/// The message line of one action. Each action has its own signal, so a refusal shows only
/// where it was asked for.
fn action_error(signal: &str) -> Markup {
	html! {
		p class="text-sm text-destructive" role="alert" "data-show"=(format!("${signal}")) "data-text"=(format!("${signal}")) style="display: none" {}
	}
}

fn fact(label: &str, value: &Markup) -> Markup {
	html! {
		div class="ui-fact" {
			dt { (label) }
			dd { (value) }
		}
	}
}

/// One cell of the hardware strip: an icon, what it is, the figure and a line under it.
struct HwCell {
	icon: &'static str,
	key: &'static str,
	value: String,
	sub: String,
}

/// The cells of the strip, from what the agent last reported. Empty until it has.
fn hardware_cells(server: &Server) -> Vec<HwCell> {
	let Some(hw) = server.hardware.as_ref().map(|h| &h.0) else {
		return Vec::new();
	};
	let mut cells = Vec::new();
	if hw.cpu.threads > 0 || !hw.cpu.model.is_empty() {
		cells.push(HwCell {
			icon: "cpu",
			key: "CPU",
			value: hw.cpu.model.clone(),
			sub: format!("{} threads", hw.cpu.threads),
		});
	}
	if hw.memory.total_mb > 0 {
		cells.push(HwCell {
			icon: "memory-stick",
			key: "RAM",
			value: human_mb(hw.memory.total_mb),
			sub: format!("{} available", human_mb(hw.memory.available_mb)),
		});
	}
	for gpu in &hw.gpus {
		cells.push(HwCell {
			icon: "zap",
			key: "GPU",
			value: gpu.name.clone(),
			sub: match gpu.vram_used_mb {
				Some(used) => format!("{} VRAM · {} in use", human_mb(gpu.vram_mb), human_mb(used)),
				None => format!("{} VRAM", human_mb(gpu.vram_mb)),
			},
		});
	}
	if let Some(disk) = hw.disks.iter().max_by_key(|d| d.total_mb) {
		cells.push(HwCell {
			icon: "hard-drive",
			key: "Disk",
			value: human_mb(disk.total_mb),
			sub: format!("{} free", human_mb(disk.free_mb)),
		});
	}
	cells
}

/// The hardware strip: hairline cells, no cards.
fn hardware_strip(server: &Server) -> Markup {
	let cells = hardware_cells(server);
	html! {
		@if cells.is_empty() {
			p class="ui-lede" { "Hardware not reported yet." }
		} @else {
			ul class="ui-hw" aria-label="Hardware" {
				@for c in &cells {
					li class="ui-hw-item" {
						(icon(c.icon))
						div class="ui-hw-text" {
							p class="ui-hw-k" { (c.key) }
							p class="ui-hw-v" title=(c.value) { (c.value) }
							p class="ui-hw-s" { (c.sub) }
						}
					}
				}
			}
		}
	}
}

/// Status, message, hardware and facts of one server; patched in place by the poll.
fn status_region(ctx: &TeamCtx, server: &Server, job: Option<&Job>) -> Markup {
	let fingerprint = server.host_key.as_deref().and_then(host_key_fingerprint);
	let url = format!("{}/status", detail_url(ctx.team.id, server.id));
	let polling = server.status.in_flight() || job_in_flight(job);
	html! {
		div id="server-status" class="ui-stack" {
			div class="ui-meta" {
				(status_badge(server.status))
				@if let Some(job) = job {
					span class="ui-lede" { "Bootstrap " (job_label(job.status)) }
				}
			}
			@if let Some(message) = &server.status_message {
				p class="ui-err" { (message) }
			}
			(hardware_strip(server))
			dl class="ui-facts" {
				(fact("Address", &html! { (host_port(server)) }))
				@if let Some(user) = &server.ssh_user {
					(fact("SSH user", &html! { (user) }))
				}
				(fact("Podman socket", &html! { span class="font-mono" { (server.podman_socket_path) } }))
				(fact("Agent version", &html! { (server.agent_version.as_deref().unwrap_or("not installed yet")) }))
				(fact("Architecture", &html! { (server.arch.as_deref().unwrap_or("unknown")) }))
				@if !server.is_local {
					(fact("Host key", &html! {
						@if let Some(fp) = &fingerprint {
							span class="font-mono text-xs break-all" { (fp) }
						} @else {
							"not pinned yet; the next bootstrap scans it"
						}
					}))
				}
			}
			(poll(&url, polling))
		}
	}
}

/// Re-run, reset host key and delete: the page's actions. The last two open a confirm.
fn actions(ctx: &TeamCtx, server: &Server, can_update: bool, can_delete: bool) -> Markup {
	let base = detail_url(ctx.team.id, server.id);
	let rerun = format!("@post('{base}/bootstrap')");
	html! {
		@if can_update {
			button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(rerun) "data-indicator:_busy" "data-attr:disabled"="$_busy" { (icon("refresh-cw")) "Re-run bootstrap" }
			(dialog_open_button(RESET_DIALOG, BUTTON_OUTLINE, &html! { (icon("key-round")) "Reset host key" }))
		}
		@if can_delete {
			(dialog_open_button(DELETE_DIALOG, BUTTON_DANGER, &html! { (icon("trash-2")) "Delete server" }))
		}
	}
}

fn reset_dialog(ctx: &TeamCtx, server: &Server) -> Markup {
	let reset = format!(
		"@post('{}/bootstrap?reset_host_key=true')",
		detail_url(ctx.team.id, server.id)
	);
	dialog(
		RESET_DIALOG,
		"Reset the host key?",
		None,
		&html! {
			p {
				"podkit pins the server's SSH host key the first time it connects and refuses any other key afterwards. "
				"Reset it only when you know the host changed, for example after reinstalling its OS. "
				"If you did not change the host, a different key can mean someone is intercepting the connection."
			}
		},
		&html! {
			form method="dialog" {
				button class=(BUTTON_OUTLINE) autofocus { "Cancel" }
			}
			button type="button" class=(BUTTON_DANGER) "data-on:click"=(reset) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Forget the key and bootstrap" }
		},
	)
}

fn delete_dialog(ctx: &TeamCtx, server: &Server) -> Markup {
	let post = format!("@post('{}/delete')", detail_url(ctx.team.id, server.id));
	dialog(
		DELETE_DIALOG,
		"Delete server",
		Some("podkit forgets this server. Nothing is removed from the host itself."),
		&html! {
			div class="flex flex-col gap-3" {
				p { "Type " strong { (server.name) } " to confirm." }
				input class=(INPUT) type="text" autocomplete="off" aria-label="Server name" "data-bind:confirm";
				(action_error("_delete_error"))
			}
		},
		&html! {
			(dialog_cancel(BUTTON_OUTLINE, &html! { "Cancel" }))
			button type="button" class=(BUTTON_DANGER) "data-on:click"=(post) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Delete server" }
		},
	)
}

/// The header every page of a server shares: logo, name, address, the page's actions and
/// the two tabs.
pub(in crate::ui) fn header(
	team: TeamId,
	server: &Server,
	tab: ServerTab,
	actions: &Markup,
) -> Markup {
	html! {
		div class="ui-stack" {
			header class="ui-page-head" {
				div class="ui-head-id" {
					(logo(&server.name, LogoSize::Lg))
					div class="ui-page-title" {
						p class="ui-eyebrow" { "Server · " (host_port(server)) }
						h1 { (server.name) }
					}
					@if server.is_local { (local_badge()) }
				}
				div class="ui-page-actions" { (actions) }
			}
			(server_tabs(team, server.id, tab))
		}
	}
}

async fn detail(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
) -> Result<Markup, Response> {
	let found = service::get(&s.db, ctx.user.id, ctx.team.id, id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let server = &found.server;
	let can_update = ctx.perms.can(Action::Update, Resource::Server) && !server.is_local;
	let can_delete = ctx.perms.can(Action::Delete, Resource::Server) && !server.is_local;
	let log_url = format!("{}/log", detail_url(ctx.team.id, server.id));
	let scan_settings = if security::can_see(&ctx) {
		crate::scanning::service::settings(&s.db, ctx.user.id, ctx.team.id, server.id)
			.await
			.ok()
	} else {
		None
	};

	let mut crumbs = crumbs(&ctx);
	crumbs.push((server.name.as_str(), None));
	let body = html! {
		div class="ui-page-body" data-signals="{confirm: '', _busy: false, _error: '', _delete_error: ''}" {
			(header(ctx.team.id, server, ServerTab::Overview, &actions(&ctx, server, can_update, can_delete)))
			(form_error())
			(status_region(&ctx, server, found.bootstrap.as_ref()))
			@if let Some(settings) = scan_settings {
				(security::settings_card(&ctx, server.id, settings))
			}
			@if !server.is_local {
				section class="ui-stack" aria-labelledby="bootstrap-title" {
					h2 id="bootstrap-title" class="ui-sec-h" { "Bootstrap log" }
					@if found.bootstrap.is_some() {
						p class="ui-lede" { "Re-running reinstalls the agent and refreshes its service unit over SSH." }
						(log_view(LOG_ID, &log_url))
					} @else {
						p class="ui-lede" { "No bootstrap has run yet." }
					}
				}
			}
			@if can_update { (reset_dialog(&ctx, server)) }
			@if can_delete { (delete_dialog(&ctx, server)) }
		}
	};
	Ok(app_page(&ctx, &server.name, &crumbs, &body))
}

/// The polled re-render of the status region.
async fn status(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
) -> Response {
	match service::get(&s.db, ctx.user.id, ctx.team.id, id).await {
		Ok(found) => sse::patch(&status_region(
			&ctx,
			&found.server,
			found.bootstrap.as_ref(),
		))
		.into_response(),
		Err(e) => error::action(&e),
	}
}

/// The newest bootstrap job's log as Datastar patches.
async fn bootstrap_log(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
) -> Response {
	match service::get(&s.db, ctx.user.id, ctx.team.id, id).await {
		Ok(found) => job_log(&s, found.bootstrap.map(|j| j.id), LOG_ID),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize)]
struct BootstrapQuery {
	#[serde(default)]
	reset_host_key: bool,
}

async fn bootstrap(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
	Query(query): Query<BootstrapQuery>,
) -> Response {
	match service::bootstrap(&s, ctx.user.id, ctx.team.id, id, query.reset_host_key).await {
		// A reload shows the new job's log from its first line.
		Ok(server) => sse::redirect(&detail_url(ctx.team.id, server.id)).into_response(),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize)]
struct DeleteSignals {
	confirm: String,
}

async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
	ReadSignals(sig): ReadSignals<DeleteSignals>,
) -> Response {
	let result = async {
		// The name is checked after a read so a mistyped name never reaches
		// the delete; `remove` still authorizes the delete itself.
		let found = service::get(&s.db, ctx.user.id, ctx.team.id, id).await?;
		if sig.confirm != found.server.name {
			return Ok(None);
		}
		service::remove(&s, ctx.user.id, ctx.team.id, id).await?;
		Ok::<_, ServerError>(Some(found.server))
	}
	.await;
	match result {
		Ok(Some(_)) => sse::redirect(&list_url(ctx.team.id))
			.and(sse::signals(&json!({ "confirm": "", "_delete_error": "" })))
			.into_response(),
		Ok(None) => delete_error("The name doesn't match. Type it exactly to confirm."),
		Err(e) => delete_error(&error::message(&e)),
	}
}

fn delete_error(message: &str) -> Response {
	sse::signals(&json!({ "_delete_error": message })).into_response()
}
