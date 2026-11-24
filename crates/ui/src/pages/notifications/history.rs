//! The bell's page: the team's notification history, newest first, with what the signed-in
//! person has not read marked. Any member may open it; marking read only touches their own
//! state.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use maud::{Markup, html};
use serde_json::json;
use time::OffsetDateTime;

use crate::components::{
	BUTTON_OUTLINE, Size, Tone, Variant, badge, card, card_section, empty_state, icon, link_button,
};
use crate::extract::TeamCtx;
use crate::layout::app_page;
use crate::pages::projects::cards::relative_time;
use crate::sse::js_string;
use crate::{error, sse};
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::ids::{NotificationEventId, TeamId};
use podkit_core::notifications::Severity;
use podkit_core::notifications::service::{self, FeedItem};
use podkit_core::state::AppState;

const LIMIT: i64 = 100;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/notifications", get(page))
		.route("/teams/{team_id}/notifications/read-all", post(read_all))
		.route(
			"/teams/{team_id}/notifications/{event_id}/read",
			post(read_one),
		)
}

fn url(team: TeamId) -> String {
	format!("/teams/{team}/notifications")
}

pub fn tone(severity: Severity) -> Tone {
	match severity {
		Severity::Info => Tone::Neutral,
		Severity::Warning => Tone::Warning,
		Severity::Critical => Tone::Danger,
	}
}

/// A link target inside podkit, or nothing: what is stored is ours, but a page that links to
/// whatever a row says would be one bad row from an open redirect.
fn local(link: Option<&str>) -> Option<&str> {
	link.filter(|l| l.starts_with('/') && !l.starts_with("//") && !l.contains('\\'))
}

fn item(team: TeamId, note: &FeedItem, now: OffsetDateTime) -> Markup {
	let severity = Severity::from_rank(note.severity);
	let read = format!(
		"@post({})",
		js_string(&format!("{}/{}/read", url(team), note.id))
	);
	html! {
		li class="ui-note" data-unread[note.unread] {
			div class="ui-note-main" {
				h2 class="ui-note-title" {
					(note.title)
					(badge(tone(severity), severity.label()))
					@if note.unread { span class="ui-tag ui-tag-progress" { "New" } }
				}
				@if !note.body.is_empty() { p class="ui-note-body" { (note.body) } }
				div class="ui-note-meta" {
					(relative_time(note.last_at, now))
					@if note.count > 1 { span { "Happened " (note.count) " times" } }
				}
			}
			div class="ui-note-act" {
				@if let Some(link) = local(note.link.as_deref()) {
					a class=(crate::components::button_class(Variant::Outline, Size::Sm)) href=(link) { "Open" }
				}
				@if note.unread {
					button type="button" class=(crate::components::button_class(Variant::Ghost, Size::Sm)) "data-on:click"=(read) {
						"Mark read"
					}
				}
			}
		}
	}
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let items = service::feed(&s.db, ctx.user.id, ctx.team.id, LIMIT)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let unread = items.iter().filter(|i| i.unread).count();
	let now = OffsetDateTime::now_utc();
	let read_all = format!(
		"@post({})",
		js_string(&format!("{}/read-all", url(ctx.team.id)))
	);
	let manage = ctx.perms.can(Action::Read, Resource::Notification);
	let body = html! {
		div class="ui-page-body" {
			header class="ui-page-head" {
				div class="ui-page-title" {
					h1 { "Notifications" }
					p class="ui-lede" {
						"What happened in " (ctx.team.name) ": deploys, servers, backups, alerts. "
						"Where else it is sent is decided by rules."
					}
				}
				div class="flex flex-wrap gap-2" {
					@if unread > 0 {
						button type="button" class=(BUTTON_OUTLINE) "data-on:click"=(read_all) {
							(icon("check")) "Mark all as read"
						}
					}
					@if manage {
						(link_button(Variant::Outline, Size::Md,
							&format!("/teams/{}/settings/notifications", ctx.team.id),
							&html! { (icon("settings")) "Channels and rules" }))
					}
				}
			}
			@if items.is_empty() {
				(empty_state(
					"bell",
					"Nothing yet.",
					Some("Failed deploys, unreachable servers, backups and metric alerts show up here."),
					None,
				))
			} @else {
				(card(&html! {
					(card_section("History", Some("The newest 100 events, newest first."), None))
					ul class="ui-notes" {
						@for note in &items { (item(ctx.team.id, note, now)) }
					}
				}))
			}
		}
	};
	Ok(app_page(
		&ctx,
		"Notifications",
		&[("Notifications", None)],
		&body,
	))
}

async fn read_all(State(s): State<AppState>, ctx: TeamCtx) -> Response {
	match service::mark_all_read(&s.db, ctx.user.id, ctx.team.id).await {
		Ok(()) => sse::redirect(&url(ctx.team.id)).into_response(),
		Err(e) => error::action(&e),
	}
}

async fn read_one(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, event)): Path<(TeamId, NotificationEventId)>,
) -> Response {
	match service::mark_read(&s.db, ctx.user.id, ctx.team.id, event).await {
		Ok(()) => sse::redirect(&url(ctx.team.id)).into_response(),
		Err(e) => sse::signals(&json!({ "_error": error::message(&e) })).into_response(),
	}
}
