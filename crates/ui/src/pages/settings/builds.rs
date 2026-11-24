//! Settings → Builds: which server builds, what a build may use, and each server's build
//! cache.
//!
//! One form with one save bar: the team's build server and caps, then for every server how
//! many builds it runs at once and how large its layer cache may stay. A server's cache size
//! is asked of its agent when the page opens (a server that does not answer shows "not
//! reachable" instead of slowing the page). "Prune" and "Clear" act at once and answer in
//! place: they are not part of the form, so they never touch what is typed in it.
//!
//! The service authorizes: reading needs `read server`, saving and pruning `update server`.
//! Someone who may not update sees the same facts as text.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde_json::{Map, Value, json};

use super::frame::settings_page;
use crate::components::{
	INPUT, LABEL, SELECT, Size, Tone, Variant, badge, button_class, card, card_content,
	card_heading, empty_state, save_bar, save_bar_saved,
};
use crate::extract::TeamCtx;
use crate::pages::apps::chrome::bytes;
use crate::sse::{self, js_string};
use crate::{error, shell};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::builds::service::{self, PruneTo, ServerInput, ServerRow, TeamInput, TeamView};
use podkit_core::ids::{ServerId, TeamId};
use podkit_core::servers::ServerStatus;
use podkit_core::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/settings/builds", get(page).post(save))
		.route(
			"/teams/{team_id}/settings/builds/servers/{server_id}/prune",
			post(prune),
		)
		.route(
			"/teams/{team_id}/settings/builds/servers/{server_id}/clear",
			post(clear),
		)
}

/// Help under a field.
const HELP: &str = "text-xs font-normal text-muted-foreground";

fn url(team: TeamId) -> String {
	format!("/teams/{team}/settings/builds")
}

/// The signal of a server's field.
fn signal(server: ServerId, field: &str) -> String {
	format!("s{server}_{field}")
}

/// Every signal the form's fields are bound to; the save bar watches them all.
fn field_names(view: &TeamView) -> Vec<String> {
	["build_server", "build_cpu", "build_memory"]
		.into_iter()
		.map(str::to_owned)
		.chain(
			view.servers
				.iter()
				.flat_map(|row| [signal(row.server.id, "max"), signal(row.server.id, "cache")]),
		)
		.collect()
}

/// The form's starting values.
fn initial(view: &TeamView) -> Value {
	let mut signals = Map::new();
	let team = &view.settings;
	signals.insert(
		"build_server".into(),
		json!(
			team.build_server_id
				.map(|s| s.0.to_string())
				.unwrap_or_default()
		),
	);
	signals.insert(
		"build_cpu".into(),
		json!(team.cpu_limit.map(|c| c.to_string()).unwrap_or_default()),
	);
	signals.insert(
		"build_memory".into(),
		json!(
			team.memory_limit_mb
				.map(|m| m.to_string())
				.unwrap_or_default()
		),
	);
	for row in &view.servers {
		signals.insert(
			signal(row.server.id, "max"),
			json!(row.settings.max_concurrent.to_string()),
		);
		signals.insert(
			signal(row.server.id, "cache"),
			json!(row.settings.cache_max_mb.to_string()),
		);
	}
	signals.insert("_error".into(), json!(""));
	Value::Object(signals)
}

fn number_input(label: &str, signal: &str, placeholder: &str, help: &str) -> Markup {
	html! {
		label class=(LABEL) {
			(label)
			input class=(INPUT) type="text" inputmode="numeric" pattern="[0-9]*\\.?[0-9]*" autocomplete="off"
				enterkeyhint="next" placeholder=(placeholder) "data-bind"=(signal);
			span class=(HELP) { (help) }
		}
	}
}

/// What a server's cache takes, in a cell the prune buttons patch.
fn cache_cell(row: &ServerRow) -> Markup {
	html! {
		span id={ "build-cache-" (row.server.id) } {
			@match &row.cache {
				Some(cache) => { (bytes(i64::try_from(cache.bytes).unwrap_or(i64::MAX))) " in " (cache.layers) " unused layers" }
				None => { "not reachable" }
			}
		}
	}
}

fn server_block(base: &str, row: &ServerRow, can_update: bool) -> Markup {
	let id = row.server.id;
	let active = row.server.status == ServerStatus::Active;
	let (tone, word) = if active {
		(Tone::Success, "Active")
	} else {
		(Tone::Warning, "Not ready")
	};
	let act = |path: &str| {
		format!(
			"evt.preventDefault(); @post({})",
			js_string(&format!("{base}/servers/{id}/{path}"))
		)
	};
	html! {
		section class="ui-build-server" aria-labelledby={ "build-server-" (id) } {
			div class="flex flex-wrap items-center gap-3" {
				h3 id={ "build-server-" (id) } class="m-0 text-sm font-medium" { (row.server.name) }
				(badge(tone, word))
				span class="text-sm text-muted-foreground" {
					"Cache: " (cache_cell(row)) " · running " (row.running) " build" @if row.running != 1 { "s" }
				}
			}
			@if can_update {
				div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
					(number_input("Builds at once", &signal(id, "max"), "2", "More wait for a free place."))
					(number_input("Cache size (MB)", &signal(id, "cache"), "10240", "Pruned to this after each build. 0 keeps only what images use."))
				}
				div class="flex flex-wrap gap-2" {
					button type="button" class=(button_class(Variant::Outline, Size::Sm)) disabled[!active]
						"data-on:click"=(act("prune")) { "Prune to the size" }
					button type="button" class=(button_class(Variant::Outline, Size::Sm)) disabled[!active]
						"data-on:click"=(act("clear")) { "Clear the cache" }
				}
			} @else {
				p class="m-0 text-sm" {
					"Up to " (row.settings.max_concurrent) " builds at once; the cache is pruned to "
					(row.settings.cache_max_mb) " MB."
				}
			}
		}
	}
}

fn read_only_team(view: &TeamView) -> Markup {
	let name = |id: ServerId| {
		view.servers
			.iter()
			.find(|r| r.server.id == id)
			.map_or_else(|| "a removed server".to_owned(), |r| r.server.name.clone())
	};
	html! {
		dl class="grid grid-cols-1 gap-x-6 gap-y-4 text-sm sm:grid-cols-3" {
			div class="flex flex-col gap-1" {
				dt class="text-muted-foreground" { "Build server" }
				dd class="m-0 font-medium" { @match view.settings.build_server_id { Some(id) => { (name(id)) } None => "Each app builds where it runs" } }
			}
			div class="flex flex-col gap-1" {
				dt class="text-muted-foreground" { "CPU cap" }
				dd class="m-0 font-medium" { @match view.settings.cpu_limit { Some(c) => { (c) " cores" } None => "No cap" } }
			}
			div class="flex flex-col gap-1" {
				dt class="text-muted-foreground" { "Memory cap" }
				dd class="m-0 font-medium" { @match view.settings.memory_limit_mb { Some(m) => { (m) " MB" } None => "No cap" } }
			}
		}
	}
}

fn team_fields(view: &TeamView) -> Markup {
	html! {
		label class=(LABEL) {
			"Build server"
			select class=(SELECT) "data-bind:build_server" {
				button { selectedcontent {} }
				option value="" { "The server the app runs on" }
				@for row in view.servers.iter().filter(|r| r.server.status == ServerStatus::Active) {
					option value=(row.server.id) { (row.server.name) }
				}
			}
			span class=(HELP) {
				"Apps built from a repository are built here and sent to the server that runs them, "
				"so a build does not slow down what is serving. An app can choose its own on its Build tab."
			}
		}
		div class="grid grid-cols-1 gap-4 sm:grid-cols-2" {
			(number_input("CPU cap (cores)", "build_cpu", "No cap", "What a build may use of the server that builds."))
			(number_input("Memory cap (MB)", "build_memory", "No cap", "At least 256. Above 2048 the cap is 2048."))
		}
	}
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Response {
	let view = match service::team_view(&s, ctx.user.id, ctx.team.id).await {
		Ok(v) => v,
		Err(e) => return error::page(&ctx, &e),
	};
	let can_update = ctx.perms.can(Action::Update, Resource::Server);
	let base = url(ctx.team.id);
	let names = field_names(&view);
	let names: Vec<&str> = names.iter().map(String::as_str).collect();
	let servers = html! {
		@if view.servers.is_empty() {
			(empty_state("server", "No servers", Some("Servers appear here once the team has one."), None))
		} @else {
			div class="flex flex-col gap-6" {
				@for row in &view.servers { (server_block(&base, row, can_update)) }
			}
		}
	};
	let body = if can_update {
		html! {
			form class="flex flex-col gap-6" data-signals=(initial(&view).to_string())
				"data-on:submit"={ "evt.preventDefault(); @post(" (js_string(&base)) ")" } {
				(card(&html! {
					(card_heading("Where builds run", Some("Applies from the next deploy.")))
					(card_content(&html! { div class="flex flex-col gap-4" { (team_fields(&view)) } }))
				}))
				(card(&html! {
					(card_heading("Build servers", Some("How many builds a server runs and how much cache it keeps.")))
					(card_content(&servers))
				}))
				(save_bar(&names))
			}
		}
	} else {
		html! {
			div class="flex flex-col gap-6" {
				(card(&html! {
					(card_heading("Where builds run", None))
					(card_content(&read_only_team(&view)))
				}))
				(card(&html! {
					(card_heading("Build servers", None))
					(card_content(&servers))
				}))
			}
		}
	};
	settings_page(&ctx, "Builds", &body).into_response()
}

fn text(signals: &Value, name: &str) -> String {
	match signals.get(name) {
		Some(Value::String(s)) => s.clone(),
		Some(Value::Number(n)) => n.to_string(),
		_ => String::new(),
	}
}

async fn save(
	State(s): State<AppState>,
	ctx: TeamCtx,
	cookies: tower_cookies::Cookies,
	ReadSignals(sig): ReadSignals<Value>,
) -> Response {
	let result = async {
		let team = TeamInput {
			build_server: text(&sig, "build_server"),
			cpu: text(&sig, "build_cpu"),
			memory_mb: text(&sig, "build_memory"),
		};
		let servers = podkit_core::servers::service::list(&s.db, ctx.user.id, ctx.team.id).await?;
		let per_server: Vec<(ServerId, ServerInput)> = servers
			.iter()
			.map(|server| {
				(
					server.id,
					ServerInput {
						max_concurrent: text(&sig, &signal(server.id, "max")),
						cache_max_mb: text(&sig, &signal(server.id, "cache")),
					},
				)
			})
			// A server the page did not know (added since) has no fields: left alone.
			.filter(|(_, i)| !(i.max_concurrent.is_empty() && i.cache_max_mb.is_empty()))
			.collect();
		service::save_all(&s, ctx.user.id, ctx.team.id, &team, &per_server).await
	}
	.await;
	match result {
		Ok(()) => {
			shell::flash(&cookies, Tone::Success, "Build settings saved.");
			sse::signals(&json!({ "_error": "" }))
				.and(save_bar_saved())
				.and(sse::redirect(&url(ctx.team.id)))
				.into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn run_prune(
	s: AppState,
	ctx: TeamCtx,
	Path((_, server)): Path<(TeamId, ServerId)>,
	to: PruneTo,
) -> Response {
	match service::prune_cache(&s, ctx.user.id, ctx.team.id, server, to).await {
		Ok(pruned) => {
			let cell = html! {
				span id={ "build-cache-" (server) } {
					(bytes(i64::try_from(pruned.remaining_bytes).unwrap_or(i64::MAX)))
				}
			};
			let said = format!(
				"Removed {} layer{}, freed {}.",
				pruned.removed,
				if pruned.removed == 1 { "" } else { "s" },
				bytes(i64::try_from(pruned.freed_bytes).unwrap_or(i64::MAX)),
			);
			sse::patch(&cell).and(sse::toast(&said)).into_response()
		}
		Err(e) => error::action(&e),
	}
}

async fn prune(
	State(s): State<AppState>,
	ctx: TeamCtx,
	path: Path<(TeamId, ServerId)>,
) -> Response {
	run_prune(s, ctx, path, PruneTo::Limit).await
}

async fn clear(
	State(s): State<AppState>,
	ctx: TeamCtx,
	path: Path<(TeamId, ServerId)>,
) -> Response {
	run_prune(s, ctx, path, PruneTo::Empty).await
}
