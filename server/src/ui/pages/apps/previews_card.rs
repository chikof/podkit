//! The Previews section of an application's Git tab: the settings (part of the tab's one form
//! and its one save bar) and the list of previews with a button to remove one.
//!
//! Everything is read and written through `previews::service`, which authorizes and scopes.
//! The card says what a preview gets, in one screen: a fresh empty database, only the
//! variables marked "available to previews", caps lower than production.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};
use serde_json::{Value, json};

use super::chrome::dot;
use super::git_tab::{check, stamp};
use crate::auth::permission::{Action, Resource};
use crate::ids::{ApplicationId, PreviewId, ProjectId, TeamId};
use crate::previews::service::{self, View};
use crate::previews::{Preview, Settings, State as PreviewState};
use crate::state::AppState;
use crate::ui::components::{
	INPUT, LABEL, Size, Status as Shape, Tone, Variant, badge, button_class, card, card_content,
	card_section, empty_state, table,
};
use crate::ui::error;
use crate::ui::extract::TeamCtx;
use crate::ui::sse::js_string;

use super::{app_url, toast_ok};

const HELP: &str = "ui-hint";

/// The signals of the previews fields; the save bar watches all of them.
pub const FIELDS: &[&str] = &[
	"pv_enabled",
	"pv_command",
	"pv_max",
	"pv_ttl",
	"pv_mem",
	"pv_cpu",
	"pv_fork",
];

/// The fields' starting values.
pub fn signals(s: &Settings) -> Value {
	json!({
		"pv_enabled": s.enabled,
		"pv_command": s.command,
		"pv_max": s.max_concurrent.to_string(),
		"pv_ttl": s.ttl_days.to_string(),
		"pv_mem": s.memory_limit_mb.map(|n| n.to_string()).unwrap_or_default(),
		"pv_cpu": s.cpu_limit.map(|n| n.to_string()).unwrap_or_default(),
		"pv_fork": s.fork_variables,
	})
}

fn number_field(label: &str, signal: &str, help: &str, placeholder: &str) -> Markup {
	html! {
		label class=(LABEL) {
			(label)
			input class=(INPUT) type="text" inputmode="decimal" autocomplete="off" enterkeyhint="done"
				placeholder=(placeholder) "data-bind"=(signal);
			span class=(HELP) { (help) }
		}
	}
}

fn fields() -> Markup {
	html! {
		(check("pv-enabled", "pv_enabled", "Make a preview of every pull request", Some("Each pull request gets its own copy of this application at pr-<number>-<name> on your domain, with a fresh empty database. podkit comments the link on the pull request and removes everything when it closes.")))
		div class="flex flex-col gap-4" "data-show"="$pv_enabled" {
			label class=(LABEL) {
				"After it starts, run"
				input class=(INPUT) type="text" maxlength="500" autocomplete="off" autocapitalize="none" autocorrect="off"
					spellcheck="false" placeholder="npm run migrate && npm run seed" "data-bind"="pv_command";
				span class=(HELP) { "Runs once inside the preview, for migrations and seed data. The preview's database starts empty, so this is how it gets its tables." }
			}
			div class="ui-pair" {
				(number_field("Open previews at most", "pv_max", "More pull requests wait for a free place.", "3"))
				(number_field("Remove after (days idle)", "pv_ttl", "A preview with no new commit for this long is removed.", "7"))
			}
			div class="ui-pair" {
				(number_field("Memory (MB)", "pv_mem", "Empty: half of this application's limit, or 512.", "512"))
				(number_field("CPU cores", "pv_cpu", "Empty: half of this application's limit, or 0.5.", "0.5"))
			}
			(check("pv-fork", "pv_fork", "Give approved pull requests from forks the preview variables too", Some("Off: a fork gets no variables at all. Either way a fork waits until someone with write access comments /preview.")))
		}
	}
}

fn summary(view: &View) -> Markup {
	let s = &view.settings;
	html! {
		p class="text-sm" {
			@if s.enabled {
				"On: up to " (s.max_concurrent) " open previews, removed after " (s.ttl_days) " idle days."
			} @else {
				"Off."
			}
		}
	}
}

/// The settings card, inside the tab's form.
pub fn settings_card(view: &View, can_edit: bool) -> Markup {
	card(&html! {
		(card_section(
			"Previews",
			Some("A temporary copy of this application for each pull request, with its own empty database."),
			None,
		))
		(card_content(&html! {
			div class="flex flex-col gap-4" {
				@if !view.has_provider {
					p class="text-sm text-muted-foreground" {
						"This application is not cloned through a git provider, so podkit can't see pull requests or comment on them."
					}
				} @else if can_edit {
					(fields())
					p class=(HELP) {
						"Only variables marked \u{201c}available to previews\u{201d} on the Environment tab are copied"
						@if view.shared_variables > 0 { " (" (view.shared_variables) " now)" }
						". Previews never join your networks."
					}
				} @else {
					(summary(view))
				}
			}
		}))
	})
}

fn shape(state: PreviewState) -> Shape {
	match state {
		PreviewState::Ready => Shape::Running,
		PreviewState::Building | PreviewState::Queued => Shape::Building,
		PreviewState::Failed => Shape::Failed,
		PreviewState::AwaitingApproval | PreviewState::Limited => Shape::Pending,
		PreviewState::Removing => Shape::Stopped,
	}
}

/// Only a link podkit itself made is shown as one.
fn safe_link(url: &str) -> Option<&str> {
	(url.starts_with("http://") || url.starts_with("https://")).then_some(url)
}

/// The previews card. Its id is what the handlers patch.
pub fn list_card(base: &str, previews: &[Preview], can_remove: bool) -> Markup {
	let rows: Vec<Vec<Markup>> = previews
		.iter()
		.map(|p| {
			vec![
				html! {
					span class="flex min-w-0 flex-col gap-1" {
						span class="font-medium" { "#" (p.number) }
						@if !p.title.is_empty() { span class="text-muted-foreground break-words" { (p.title) } }
					}
				},
				html! {
					span class="flex flex-col gap-1" {
						span class="flex flex-wrap items-center gap-2" {
							(dot(shape(p.state()), p.state().label()))
							@if p.from_fork { (badge(Tone::Warning, "Fork")) }
						}
						@if !p.detail.is_empty() { span class="text-muted-foreground break-words" { (p.detail) } }
					}
				},
				html! {
					@if let Some(url) = safe_link(&p.url) {
						a class="break-all underline" href=(url) target="_blank" rel="noopener noreferrer" { (url) }
					}
				},
				html! { time datetime=(stamp(p.last_activity_at)) { (stamp(p.last_activity_at)) } },
				html! {
					@if can_remove && p.state() != PreviewState::Removing {
						button type="button" class=(button_class(Variant::Outline, Size::Sm))
							"data-on:click"=(format!("@post({})", js_string(&format!("{base}/previews/{}/remove", p.id)))) { "Remove" }
					}
				},
			]
		})
		.collect();
	html! {
		div id="previews-list" {
			(card(&html! {
				(card_section("Open previews", Some("Pull requests that have a preview, or are waiting for one."), None))
				(card_content(&html! {
					@if rows.is_empty() {
						(empty_state("git-pull-request", "No previews", Some("Open a pull request against this repository and its preview shows up here."), None))
					} @else {
						(table("previews-table", "Open previews", &["Pull request", "State", "Link", "Last activity", ""], &rows))
					}
				}))
			}))
		}
	}
}

pub async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app, preview)): Path<(TeamId, ProjectId, ApplicationId, PreviewId)>,
) -> Response {
	let result = async {
		service::remove(&s, ctx.user.id, ctx.team.id, project, app, preview).await?;
		service::get(&s, ctx.user.id, ctx.team.id, project, app).await
	}
	.await;
	match result {
		Ok(view) => {
			let base = app_url(ctx.team.id, project, app) + "/git";
			crate::ui::sse::patch(&list_card(
				&base,
				&view.previews,
				ctx.perms.can(Action::Delete, Resource::Application),
			))
			.and(toast_ok("Preview is being removed"))
			.into_response()
		}
		Err(e) => error::action(&e),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use time::OffsetDateTime;

	fn preview(state: &str) -> Preview {
		Preview {
			id: PreviewId(9),
			team_id: TeamId(1),
			project_id: ProjectId(1),
			application_id: ApplicationId(1),
			number: 4,
			title: "<script>alert(1)</script>".to_owned(),
			head_sha: "abc".to_owned(),
			head_ref: "x".to_owned(),
			head_repo: "o/r".to_owned(),
			from_fork: true,
			approved: false,
			state: state.to_owned(),
			detail: "<img src=x>".to_owned(),
			environment_id: None,
			preview_application_id: None,
			url: "javascript:alert(1)".to_owned(),
			comment_id: None,
			last_activity_at: OffsetDateTime::now_utc(),
			created_at: OffsetDateTime::now_utc(),
		}
	}

	#[test]
	fn previews_card_escapes_text_and_refuses_script_links() {
		let html = list_card("/b", &[preview("ready")], true).into_string();
		assert!(
			!html.contains("<script>") && !html.contains("<img"),
			"{html}"
		);
		assert!(html.contains("&lt;script&gt;"), "{html}");
		assert!(!html.contains("javascript:"), "{html}");
		assert!(html.contains("/b/previews/9/remove"), "{html}");
	}

	#[test]
	fn previews_card_has_no_remove_button_without_the_right_or_while_removing() {
		let html = list_card("/b", &[preview("ready")], false).into_string();
		assert!(!html.contains("/remove"), "{html}");
		let html = list_card("/b", &[preview("removing")], true).into_string();
		assert!(!html.contains("/remove"), "{html}");
	}

	#[test]
	fn previews_signals_cover_every_field_the_save_bar_watches() {
		let map = signals(&Settings::default());
		for field in FIELDS {
			assert!(map.get(*field).is_some(), "{field}");
		}
		assert_eq!(
			map.as_object().map_or(0, serde_json::Map::len),
			FIELDS.len()
		);
		assert_eq!(map["pv_max"], "3");
		assert_eq!(map["pv_ttl"], "7");
	}
}
