//! The audit log as the web shows it: a table of events, and the "Activity" card a resource
//! page puts beside its other sections. Both read events the audit service already loaded
//! (`audit::service`), so a page that may not read the log never gets here.
//!
//! Every value in an event is data someone typed (a name, a hostname, a user agent), so it
//! goes through maud's escaping like any other text; nothing here uses `PreEscaped`.

use maud::{Markup, html};
use time::OffsetDateTime;

use super::{card, card_content, card_heading, empty_state, table};
use crate::audit::service::actor_label;
use crate::audit::{ActorKind, Event};
use crate::ids::TeamId;

/// `2026-11-17 20:03 UTC`: no locale, no JS, the same everywhere.
#[must_use]
pub fn when(t: OffsetDateTime) -> String {
	let t = t.to_offset(time::UtcOffset::UTC);
	format!(
		"{}-{:02}-{:02} {:02}:{:02} UTC",
		t.date().year(),
		t.month() as u8,
		t.day(),
		t.hour(),
		t.minute(),
	)
}

/// The page of the resource an event is about, when there is one to go to. Deleting something
/// leaves nothing to open.
#[must_use]
pub fn href(team: TeamId, e: &Event) -> Option<String> {
	if e.action.ends_with(".delete") || e.action.ends_with(".revoke") {
		return None;
	}
	let base = format!("/teams/{team}");
	let id = e.target_id;
	let project = e.project.map(|p| p.0);
	let in_project = |tail: &str| project.map(|p| format!("{base}/projects/{p}{tail}"));
	let diff_id = |key: &str| e.diff.get(key).and_then(serde_json::Value::as_i64);
	match (e.target_kind.as_str(), id) {
		("application", Some(id)) => in_project(&format!("/apps/{id}")),
		("database", Some(id)) => in_project(&format!("/databases/{id}")),
		("stack", Some(id)) => in_project(&format!("/stacks/{id}")),
		("model", Some(id)) => in_project(&format!("/models/{id}")),
		("project" | "environment", _) => in_project(""),
		("env_var", _) => in_project(&format!("/apps/{}/env", diff_id("application_id")?)),
		("domain", _) => in_project(&format!("/apps/{}/domains", diff_id("application_id")?)),
		("volume", _) => in_project(&format!("/apps/{}/volumes", diff_id("application_id")?)),
		("backup", _) => in_project(&format!("/databases/{}", diff_id("database_id")?)),
		("server", Some(id)) => Some(format!("{base}/servers/{id}")),
		("ssh_key", _) => Some(format!("{base}/keys")),
		("git_provider", _) => Some(format!("{base}/git-providers")),
		("api_token", _) => Some(format!("{base}/settings/api-tokens")),
		("member", _) => Some(format!("{base}/members")),
		("team", _) => Some(format!("{base}/settings")),
		("model_file", Some(id)) => Some(format!("{base}/models/downloads/{id}")),
		("huggingface_token", _) => Some(format!("{base}/models/settings")),
		("scan_ignore", _) => Some(format!("{base}/security")),
		("audit", _) => Some(format!("{base}/settings/audit")),
		_ => None,
	}
}

fn who(e: &Event) -> Markup {
	let tag = match e.actor_kind {
		ActorKind::User => None,
		ActorKind::Token => Some("API token"),
		ActorKind::System => Some("Automatic"),
		ActorKind::Mcp => Some("MCP"),
	};
	html! {
		(actor_label(e))
		@if let Some(tag) = tag {
			small { (tag) }
		}
	}
}

fn what(team: TeamId, e: &Event) -> Markup {
	let pretty = e
		.diff
		.as_object()
		.filter(|o| !o.is_empty())
		.and_then(|_| serde_json::to_string_pretty(&e.diff).ok());
	html! {
		@if let Some(link) = href(team, e) {
			a href=(link) class="ui-link" { (e.summary) }
		} @else {
			(e.summary)
		}
		small class="ui-mono" { (e.action) }
		@if let Some(pretty) = pretty {
			details class="ui-audit-diff" {
				summary { "Details" }
				pre { code { (pretty) } }
			}
		}
	}
}

fn from(e: &Event) -> Markup {
	html! {
		@if let Some(ip) = &e.ip { span class="ui-mono" { (ip) } } @else { "–" }
		@if let Some(ua) = &e.user_agent {
			small title=(ua) class="ui-audit-ua" { (ua) }
		}
	}
}

/// The events as a table, newest first. `id` is the element id of the table's region; `caption`
/// names it for assistive technology.
#[must_use]
pub fn audit_table(id: &str, caption: &str, team: TeamId, events: &[Event]) -> Markup {
	let rows: Vec<Vec<Markup>> = events
		.iter()
		.map(|e| {
			vec![
				html! { time datetime=(e.created_at.unix_timestamp()) { (when(e.created_at)) } },
				who(e),
				what(team, e),
				from(e),
			]
		})
		.collect();
	table(id, caption, &["When", "Who", "What", "From"], &rows)
}

/// The Activity card of a resource page: its latest events, and a way to the whole log.
#[must_use]
pub fn activity_card(team: TeamId, events: &[Event], all: &str) -> Markup {
	card(&html! {
		(card_heading("Activity", Some("Who changed this, and when. The newest events first.")))
		(card_content(&html! {
			@if events.is_empty() {
				(empty_state(
					"history",
					"Nothing yet.",
					Some("Changes made to this from now on show up here."),
					None,
				))
			} @else {
				(audit_table("activity", "Activity", team, events))
				p class="ui-hint" { a href=(all) class="ui-link" { "See the whole audit log" } }
			}
		}))
	})
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::*;
	use crate::ids::{AuditEventId, ProjectId};

	fn event(kind: &str, action: &str, id: Option<i64>, diff: serde_json::Value) -> Event {
		Event {
			id: AuditEventId(1),
			actor_kind: ActorKind::User,
			actor_user: None,
			actor_name: Some("<b>Alice</b>".to_owned()),
			actor_token: None,
			actor_token_name: None,
			action: action.to_owned(),
			target_kind: kind.to_owned(),
			target_id: id,
			project: Some(ProjectId(5)),
			summary: "Deployed application <script>x</script>".to_owned(),
			diff,
			ip: Some("203.0.113.7".to_owned()),
			user_agent: Some("agent\"><img src=x>".to_owned()),
			created_at: OffsetDateTime::UNIX_EPOCH,
		}
	}

	#[test]
	fn what_people_typed_is_escaped() {
		let e = event(
			"application",
			"app.deploy",
			Some(9),
			json!({ "name": "</code><script>alert(1)</script>" }),
		);
		let html = audit_table("t", "Events", TeamId(1), &[e]).into_string();
		assert!(!html.contains("<script"), "{html}");
		assert!(!html.contains("<img"), "{html}");
		assert!(!html.contains("<b>Alice"), "{html}");
		assert!(html.contains("&lt;script&gt;"), "{html}");
	}

	#[test]
	fn resources_link_to_their_pages_but_deleted_ones_do_not() {
		let team = TeamId(1);
		let app = event("application", "app.deploy", Some(9), json!({}));
		assert_eq!(
			href(team, &app).as_deref(),
			Some("/teams/1/projects/5/apps/9")
		);
		let gone = event("application", "app.delete", Some(9), json!({}));
		assert_eq!(href(team, &gone), None);
		let env = event(
			"env_var",
			"env_var.set",
			Some(3),
			json!({ "application_id": 9 }),
		);
		assert_eq!(
			href(team, &env).as_deref(),
			Some("/teams/1/projects/5/apps/9/env")
		);
		let server = event("server", "server.create", Some(4), json!({}));
		assert_eq!(href(team, &server).as_deref(), Some("/teams/1/servers/4"));
	}

	#[test]
	fn an_empty_activity_card_says_so() {
		let html = activity_card(TeamId(1), &[], "/teams/1/settings/audit").into_string();
		assert!(html.contains("Nothing yet."), "{html}");
		assert!(!html.contains("<table"), "{html}");
	}
}
