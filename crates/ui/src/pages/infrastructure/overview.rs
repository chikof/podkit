//! The Infrastructure overview: a **Monitor** surface. One section per server (state,
//! hardware, proxy, networks, what runs on it by project and environment, scanner, load),
//! then the table that answers "what runs where": resource, server, networks, address.
//! The setup checklist sits on top until it is done or dismissed.

use axum::extract::State;
use axum::response::Response;
use maud::{Markup, html};

use super::{busiest, crumbs, setup, setup_url};
use crate::charts::sparkline;
use crate::components::{
	LogoSize, Size, Status, Tone, Variant, badge, empty_state, link_button, logo, table,
};
use crate::error;
use crate::extract::TeamCtx;
use crate::layout::app_page;
use crate::pages::servers::{detail_url, hardware_parts, local_badge};
use crate::status::status_badge;
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::ids::TeamId;
use podkit_core::infrastructure::service::{
	self, NetworkLine, Overview, Place, ProjectGroup, Resident, ScannerLine, ServerCard,
};
use podkit_core::metrics::{Range, ServerPoint};
use podkit_core::servers::ServerStatus;
use podkit_core::state::AppState;

/// A resource's own page.
pub(super) fn resource_href(team: TeamId, project: i64, kind: &str, id: i64) -> String {
	let base = format!("/teams/{team}/projects/{project}");
	match kind {
		"application" => format!("{base}/apps/{id}"),
		"database" => format!("{base}/databases/{id}"),
		"model" => format!("{base}/models/{id}"),
		_ => format!("{base}/stacks/{id}"),
	}
}

pub(super) fn kind_label(kind: &str) -> &'static str {
	match kind {
		"application" => "Application",
		"database" => "Database",
		"model" => "Model",
		_ => "Stack",
	}
}

/// The shaped dot for a resource's state, as the shared status component draws it.
fn state_of(status: &str) -> Status {
	match status {
		"running" => Status::Running,
		"building" => Status::Building,
		"deploying" | "starting" => Status::Deploying,
		"failed" | "crashed" => Status::Failed,
		"pending" | "queued" | "not deployed" => Status::Pending,
		_ => Status::Stopped,
	}
}

/// A dot alone, with its word for assistive technology: a chip has no room for the label.
fn dot(status: &str) -> Markup {
	let s = state_of(status);
	html! {
		span class="ui-badge" data-status=(s.key()) {
			span class="ui-dot" aria-hidden="true" {}
			span class="ui-visually-hidden" { (s.label()) }
		}
	}
}

fn percent(used: i64, total: i64) -> Option<f64> {
	(total > 0).then(|| used as f64 * 100.0 / total as f64)
}

/// A labelled sparkline with its latest value in text, so the number is not only a picture.
fn figure(label: &str, values: &[Option<f64>], xs: &[i64], from: i64, to: i64) -> Markup {
	let latest = values.iter().rev().flatten().next();
	html! {
		div class="ui-infra-fig" {
			div class="ui-infra-fig-head" {
				span { (label) }
				span class="ui-infra-fig-val" {
					@if let Some(v) = latest { (format!("{v:.0}%")) } @else { "–" }
				}
			}
			(sparkline(xs, values, from, to, Some(100.0)))
		}
	}
}

fn load(points: &[ServerPoint]) -> Markup {
	let now = time::OffsetDateTime::now_utc().unix_timestamp();
	let xs: Vec<i64> = points.iter().map(|p| p.at).collect();
	let cpu: Vec<Option<f64>> = points.iter().map(|p| p.cpu).collect();
	let disk: Vec<Option<f64>> = points
		.iter()
		.map(|p| percent(p.disk_used, p.disk_total))
		.collect();
	// The line starts at the oldest reading so a young server still fills its box.
	let day_ago = now - Range::Day.duration().whole_seconds();
	let from = xs
		.first()
		.copied()
		.map_or(day_ago, |t| t.max(day_ago).min(now - 60));
	html! {
		@if points.is_empty() {
			p class="ui-row-sub" { "No readings yet." }
		} @else {
			div class="ui-infra-spark" {
				(figure("CPU, 24 h", &cpu, &xs, from, now))
				(figure("Disk, 24 h", &disk, &xs, from, now))
			}
		}
	}
}

fn proxy_text(o: &Overview, card: &ServerCard) -> Markup {
	let p = &o.proxy;
	html! {
		@if card.proxy_running {
			(badge(Tone::Success, "Traefik running"))
		} @else {
			(badge(Tone::Neutral, "Starts with the first app"))
		}
		span class="ui-infra-note" {
			"HTTP :" (p.http_port)
			@if p.acme_email.is_some() { " · HTTPS :" (p.https_port) } @else { " · HTTPS off" }
		}
	}
}

/// What a network is, for the chip's tooltip.
fn network_title(n: &NetworkLine) -> String {
	let kind = match n.kind.as_str() {
		"environment" => "Environment network",
		"system" => "System network",
		_ => "Shared network",
	};
	if n.internal {
		format!("{kind}, no outbound access")
	} else {
		kind.to_owned()
	}
}

fn networks_text(ctx: &TeamCtx, card: &ServerCard) -> Markup {
	html! {
		@if card.networks.is_empty() {
			span class="ui-infra-note" { "None yet. One appears with the first deploy." }
		} @else {
			ul class="ui-infra-chips" aria-label={ "Networks on " (card.server.name) } {
				@for n in &card.networks {
					li {
						a class="ui-infra-chip" title=(network_title(n)) href={ "/teams/" (ctx.team.id) "/infrastructure/networks#server-" (card.server.id) } {
							(n.name)
							span class="ui-infra-count" { (n.members) }
						}
					}
				}
			}
		}
	}
}

fn scanner_text(line: &ScannerLine) -> Markup {
	html! {
		@if line.enabled {
			(badge(Tone::Success, "Scanning on"))
			span class="ui-infra-note" {
				@if line.scanned == 0 { "Database downloads with the first scan" }
				@else if line.scanned == 1 { "1 image scanned" }
				@else { (line.scanned) " images scanned" }
			}
		} @else {
			(badge(Tone::Neutral, "Scanning off"))
		}
	}
}

fn residents(ctx: &TeamCtx, groups: &[ProjectGroup]) -> Markup {
	let one = |r: &Resident| {
		html! {
			li {
				a class="ui-infra-chip" href=(resource_href(ctx.team.id, r.project_id, &r.kind, r.id)) {
					(dot(&r.status))
					(r.name)
				}
			}
		}
	};
	html! {
		@if groups.is_empty() {
			p class="ui-infra-note" { "Nothing runs here yet." }
		} @else {
			div class="ui-infra-res" {
				@for g in groups {
					div class="ui-infra-proj" {
						h3 class="ui-infra-proj-name" {
							a href={ "/teams/" (ctx.team.id) "/projects/" (g.project_id) } { (g.name) }
						}
						@for e in &g.environments {
							div class="ui-infra-env" {
								span class="ui-infra-env-name" { (e.name) }
								ul class="ui-infra-chips" aria-label={ (e.name) " in " (g.name) } {
									@for r in &e.residents { (one(r)) }
								}
							}
						}
					}
				}
			}
		}
	}
}

fn server_section(ctx: &TeamCtx, o: &Overview, card: &ServerCard) -> Markup {
	let sv = &card.server;
	let hardware = hardware_parts(sv);
	let heading = format!("infra-server-{}", sv.id);
	html! {
		section class="ui-infra-server" aria-labelledby=(heading) {
			div class="ui-infra-head" {
				span class="ui-row-logo" { (logo(&sv.name, LogoSize::Md)) }
				div class="ui-row-main" {
					h2 id=(heading) class="ui-row-name" {
						a class="underline-offset-4 hover:underline" href=(detail_url(ctx.team.id, sv.id)) { (sv.name) }
					}
					p class="ui-row-sub" {
						@if hardware.is_empty() { "Hardware not reported yet." } @else { (hardware.join(" · ")) }
					}
					@if let Some(message) = &sv.status_message {
						@if sv.status != ServerStatus::Active { p class="ui-err" { (message) } }
					}
				}
				div class="ui-row-end" {
					(status_badge(sv.status))
					@if sv.is_local { (local_badge()) }
				}
			}
			dl class="ui-infra-facts" {
				div { dt { "Proxy" } dd { (proxy_text(o, card)) } }
				div { dt { "Networks" } dd { (networks_text(ctx, card)) } }
				@if let Some(line) = &card.scanner {
					div { dt { "Scanner" } dd { (scanner_text(line)) } }
				}
			}
			(residents(ctx, &card.groups))
			(load(&card.points))
		}
	}
}

fn place_row(ctx: &TeamCtx, p: &Place) -> Vec<Markup> {
	let team = ctx.team.id;
	vec![
		html! {
			span class="ui-infra-what" {
				span class="ui-infra-what-name" {
					(dot(&p.status))
					a class="font-medium underline-offset-4 hover:underline" href=(resource_href(team, p.project_id, &p.kind, p.id)) { (p.name) }
				}
				span class="ui-row-sub" { (kind_label(&p.kind)) " · " (p.project_name) " / " (p.environment_name) }
			}
		},
		html! { a class="underline-offset-4 hover:underline" href=(detail_url(team, p.server_id)) { (p.server_name) } },
		html! {
			@if p.networks.is_empty() {
				span class="text-muted-foreground" { "None" }
			} @else {
				(p.networks.join(", "))
			}
		},
		html! {
			@if let Some(url) = &p.url {
				a class="font-mono text-xs underline-offset-4 hover:underline" href=(url) { (url.trim_start_matches("http://")) }
			} @else {
				span class="text-muted-foreground" { "Internal only" }
			}
		},
	]
}

fn what_runs_where(ctx: &TeamCtx, places: &[Place]) -> Markup {
	let rows: Vec<Vec<Markup>> = places.iter().map(|p| place_row(ctx, p)).collect();
	let can_create = ctx.perms.can(Action::Create, Resource::Project);
	html! {
		section class="ui-infra-where" aria-labelledby="infra-where" {
			h2 id="infra-where" class="ui-infra-h2" { "What runs where" }
			@if rows.is_empty() {
				(empty_state(
					"rocket",
					"Nothing runs yet",
					Some("A resource shows up here with its server, its networks and its address as soon as you make one."),
					can_create.then(|| link_button(
						Variant::Default,
						Size::Md,
						&format!("/teams/{}/projects", ctx.team.id),
						&html! { "Go to projects" },
					)).as_ref(),
				))
			} @else {
				(table(
					"what-runs-where",
					"Every resource with its server, networks and address",
					&["Resource", "Server", "Networks", "Address"],
					&rows,
				))
			}
		}
	}
}

pub(super) async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let overview = service::overview(&s, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let top = podkit_core::metrics::service::busiest(&s.db, ctx.user.id, ctx.team.id)
		.await
		.unwrap_or_default();
	let body = html! {
		div class="ui-page-body" {
			header class="ui-page-head" {
				div class="ui-page-title" {
					h1 { "Infrastructure" }
					p class="ui-lede" { "What the team runs on, and where each resource lives." }
				}
				@if !overview.setup.steps.is_empty() {
					div class="ui-page-actions" {
						(link_button(Variant::Outline, Size::Md, &setup_url(ctx.team.id), &html! { "Setup" }))
					}
				}
			}
			@if overview.setup.shown() { (setup::checklist(&ctx, &overview.setup)) }
			div class="ui-infra" {
				@for card in &overview.servers { (server_section(&ctx, &overview, card)) }
				(busiest::section(&ctx, &top))
				(what_runs_where(&ctx, &overview.places))
			}
		}
	};
	Ok(app_page(&ctx, "Infrastructure", &crumbs(&ctx), &body))
}
