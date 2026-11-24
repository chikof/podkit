//! The Activity panel of an environment: what happened to its resources lately, newest first.
//! Deployments (started, live, failed, rolled back) come from the deployments table, the rest
//! (a database stopped, an application renamed) from the audit log.
//!
//! Each source is read only when the viewer may read it: deployments need `read application`
//! and `read deployment`, the log needs `read audit`. With neither there is no panel.

use maud::{Markup, html};
use time::OffsetDateTime;

use super::cards::relative_time;
use crate::audit::Event;
use crate::audit::service::{self as audit, actor_label};
use crate::auth::permission::{Action, Resource};
use crate::deployments::DeploymentStatus;
use crate::health_gate::State as GateState;
use crate::ids::{ApplicationId, TeamId};
use crate::projects::overview::{self, DeploymentRecord, EnvironmentView, short_sha};
use crate::state::AppState;
use crate::ui::components::{Status as Shape, audit_event_href, empty_state, icon};
use crate::ui::extract::TeamCtx;
use crate::ui::pages::apps::app_url;
use crate::ui::pages::apps::chrome::deployment_shape;

/// How many events the panel lists.
pub const SHOWN: usize = 8;

/// Actions of the log that a deployment row already tells, left out when the panel lists
/// deployments so nothing shows twice.
const DEPLOY_ACTIONS: [&str; 4] = [
	"app.deploy",
	"app.rollback",
	"app.restart",
	"deployment.cancel",
];

/// One line of the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
	pub at: OffsetDateTime,
	/// The resource it is about.
	pub name: String,
	/// Where it leads: the deployment, or the resource the event is about.
	pub href: Option<String>,
	/// The status shape of a deployment; `None` for a log event, drawn as a plain dot.
	pub shape: Option<Shape>,
	/// What happened, in a few words.
	pub line: String,
}

fn with_commit(word: &str, commit: Option<&str>) -> String {
	match commit {
		Some(sha) => format!("{word} {}", short_sha(sha)),
		None => word.to_owned(),
	}
}

/// What a deployment adds to the feed: the moment it started and, once it settled, how it
/// ended. One still on its way says which phase it is in. A deployment replaced by a newer
/// one ("stopped") adds no ending of its own: the newer one going live tells it.
pub fn deployment_items(
	team: TeamId,
	project: crate::ids::ProjectId,
	d: &DeploymentRecord,
) -> Vec<Item> {
	let href = Some(format!(
		"{}/deployments/{}",
		app_url(team, project, d.application),
		d.id
	));
	let commit = d.commit.as_deref();
	let item = |at, shape, line: String| Item {
		at,
		name: d.name.clone(),
		href: href.clone(),
		shape: Some(shape),
		line,
	};
	if overview::deployment_in_flight(d.status) {
		let word = if d.gate == Some(GateState::Waiting) {
			"Waiting for health"
		} else {
			crate::ui::status::Status::label(d.status)
		};
		return vec![item(
			d.created_at,
			deployment_shape(d.status),
			with_commit(word, commit),
		)];
	}
	let started = if d.rollback {
		"Rollback started"
	} else {
		"Deploy started"
	};
	let mut out = vec![item(
		d.created_at,
		Shape::Pending,
		with_commit(started, commit),
	)];
	let ending = if d.gate == Some(GateState::RolledBack) {
		Some((Shape::Failed, with_commit("Rolled back", commit)))
	} else {
		match d.status {
			DeploymentStatus::Running => Some((Shape::Running, with_commit("Live", commit))),
			DeploymentStatus::Failed | DeploymentStatus::Crashed => {
				let word = crate::ui::status::Status::label(d.status);
				Some((
					Shape::Failed,
					d.error.as_deref().map_or_else(
						|| with_commit(word, commit),
						|e| format!("{}: {}", with_commit(word, commit), first_line(e)),
					),
				))
			}
			DeploymentStatus::Cancelled => Some((Shape::Stopped, with_commit("Cancelled", commit))),
			_ => None,
		}
	};
	if let (Some((shape, line)), Some(at)) = (ending, d.finished_at) {
		out.push(item(at, shape, line));
	}
	out
}

/// The first line of an error, cut to a length a feed line can hold.
fn first_line(text: &str) -> String {
	const MAX: usize = 80;
	let line = text.lines().next().unwrap_or_default().trim();
	if line.chars().count() > MAX {
		let cut: String = line.chars().take(MAX).collect();
		format!("{}\u{2026}", cut.trim_end())
	} else {
		line.to_owned()
	}
}

/// A log event as a feed line: the resource's name as the environment shows it, the event's
/// summary and who did it.
pub fn event_item(team: TeamId, view: &EnvironmentView, e: &Event) -> Item {
	let name = if e.target_kind == "environment" {
		view.environment.name.clone()
	} else {
		view.resources
			.iter()
			.find(|c| c.kind.slug() == e.target_kind && Some(c.id) == e.target_id)
			.map_or_else(|| e.target_kind.replace('_', " "), |c| c.name.clone())
	};
	Item {
		at: e.created_at,
		name,
		href: audit_event_href(team, e),
		shape: None,
		line: format!("{} \u{b7} {}", e.summary, actor_label(e)),
	}
}

/// The newest `limit` items of both sources, newest first.
pub fn merge(mut items: Vec<Item>, limit: usize) -> Vec<Item> {
	items.sort_by_key(|i| std::cmp::Reverse(i.at));
	items.truncate(limit);
	items
}

/// What the panel's footer links to: the whole audit log for people who may read it, else
/// the deployments of the application that moved last.
fn footer_link(
	team: TeamId,
	project: crate::ids::ProjectId,
	can_audit: bool,
	newest_app: Option<ApplicationId>,
) -> Option<(&'static str, String)> {
	if can_audit {
		Some(("Audit log", format!("/teams/{team}/settings/audit")))
	} else {
		newest_app.map(|app| {
			(
				"All deployments",
				format!("{}/deployments", app_url(team, project, app)),
			)
		})
	}
}

/// The panel: a card with the feed, or what stands in for an empty one.
pub fn panel(items: &[Item], now: OffsetDateTime, link: Option<&(&str, String)>) -> Markup {
	html! {
		aside id="env-activity" class="ui-card ui-res-activity" aria-labelledby="env-activity-h" {
			div class="ui-res-act-h" {
				h2 id="env-activity-h" class="ui-card-title" { "Activity" }
				@if let Some((label, href)) = link {
					a href=(href) class="ui-link ui-res-act-all" { (label) (icon("arrow-right")) }
				}
			}
			@if items.is_empty() {
				(empty_state(
					"history",
					"Nothing yet",
					Some("Deployments and changes to this environment show up here."),
					None,
				))
			} @else {
				ol class="ui-res-feed" {
					@for item in items {
						li class="ui-res-ev" {
							@match item.shape {
								Some(shape) => span class="ui-res-ev-dot" data-status=(shape.key()) { span class="ui-dot" aria-hidden="true" {} },
								None => span class="ui-res-ev-dot" { span class="ui-dot ui-dot-neutral" aria-hidden="true" {} },
							}
							div class="ui-res-ev-main" {
								p class="ui-res-ev-name" {
									@if let Some(href) = &item.href {
										a href=(href) { (item.name) }
									} @else {
										(item.name)
									}
								}
								p class="ui-res-ev-line" { (item.line) }
							}
							span class="ui-res-ev-when" { (relative_time(item.at, now)) }
						}
					}
				}
			}
		}
	}
}

/// The panel for `view`, reading what the viewer may read; `None` when they may read
/// neither deployments nor the log. A source that fails to load leaves its items out rather
/// than the page.
pub async fn load(
	s: &AppState,
	ctx: &TeamCtx,
	view: &EnvironmentView,
	now: OffsetDateTime,
) -> Option<Markup> {
	let can_deploys = ctx.perms.can(Action::Read, Resource::Application)
		&& ctx.perms.can(Action::Read, Resource::Deployment);
	let can_audit = ctx.perms.can(Action::Read, Resource::Audit);
	if !can_deploys && !can_audit {
		return None;
	}
	let team = ctx.team.id;
	let project = view.project.id;
	let mut items = Vec::new();
	let mut newest_app = None;
	if can_deploys {
		let records = overview::recent_deployments(
			&s.db,
			ctx.user.id,
			team,
			view.environment.id,
			i64::try_from(SHOWN).unwrap_or(8),
		)
		.await
		.unwrap_or_default();
		newest_app = records.first().map(|d| d.application);
		for d in &records {
			items.extend(deployment_items(team, project, d));
		}
	}
	if can_audit {
		let mut targets: Vec<(&str, i64)> = view
			.resources
			.iter()
			.map(|c| (c.kind.slug(), c.id))
			.collect();
		targets.push(("environment", view.environment.id.0));
		let skip: &[&str] = if can_deploys { &DEPLOY_ACTIONS } else { &[] };
		let events = audit::about(
			&s.db,
			ctx.user.id,
			team,
			&targets,
			skip,
			i64::try_from(SHOWN).unwrap_or(8),
		)
		.await
		.unwrap_or_default();
		items.extend(events.iter().map(|e| event_item(team, view, e)));
	}
	let items = merge(items, SHOWN);
	let link = footer_link(team, project, can_audit, newest_app);
	Some(panel(&items, now, link.as_ref()))
}

#[cfg(test)]
mod tests {
	use time::Duration;

	use super::*;
	use crate::ids::{DeploymentId, ProjectId};

	fn record(status: DeploymentStatus, gate: Option<GateState>) -> DeploymentRecord {
		let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
		DeploymentRecord {
			id: DeploymentId(9),
			application: ApplicationId(5),
			name: "api".to_owned(),
			status,
			commit: Some("9f3c2abbbbbb".to_owned()),
			error: Some("build failed\nline two".to_owned()),
			gate,
			rollback: false,
			created_at: now - Duration::minutes(10),
			finished_at: (!overview::deployment_in_flight(status)).then_some(now),
		}
	}

	fn lines(d: &DeploymentRecord) -> Vec<String> {
		deployment_items(TeamId(1), ProjectId(2), d)
			.into_iter()
			.map(|i| i.line)
			.collect()
	}

	#[test]
	fn a_deployment_says_how_it_started_and_ended() {
		assert_eq!(
			lines(&record(DeploymentStatus::Building, None)),
			["Building 9f3c2ab"]
		);
		assert_eq!(
			lines(&record(DeploymentStatus::Running, None)),
			["Deploy started 9f3c2ab", "Live 9f3c2ab"]
		);
		assert_eq!(
			lines(&record(DeploymentStatus::Failed, None)),
			["Deploy started 9f3c2ab", "Failed 9f3c2ab: build failed"]
		);
		assert_eq!(
			lines(&record(
				DeploymentStatus::Stopped,
				Some(GateState::RolledBack)
			)),
			["Deploy started 9f3c2ab", "Rolled back 9f3c2ab"]
		);
		assert_eq!(
			lines(&record(DeploymentStatus::Stopped, None)),
			["Deploy started 9f3c2ab"]
		);
		assert_eq!(
			lines(&record(
				DeploymentStatus::Deploying,
				Some(GateState::Waiting)
			)),
			["Waiting for health 9f3c2ab"]
		);
		let items = deployment_items(
			TeamId(1),
			ProjectId(2),
			&record(DeploymentStatus::Running, None),
		);
		assert_eq!(
			items[0].href.as_deref(),
			Some("/teams/1/projects/2/apps/5/deployments/9")
		);
		assert_eq!(items[1].shape, Some(Shape::Running));
	}

	#[test]
	fn the_feed_keeps_the_newest_and_escapes_names() {
		let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
		let item = |mins: i64, name: &str| Item {
			at: now - Duration::minutes(mins),
			name: name.to_owned(),
			href: None,
			shape: None,
			line: "x".to_owned(),
		};
		let items = merge(
			vec![item(30, "old"), item(1, "new"), item(10, "<b>mid</b>")],
			2,
		);
		assert_eq!(
			items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
			["new", "<b>mid</b>"]
		);
		let html = panel(
			&items,
			now,
			Some(&("Audit log", "/teams/1/settings/audit".to_owned())),
		)
		.into_string();
		assert!(html.contains("&lt;b&gt;mid"), "{html}");
		assert!(!html.contains("<b>mid"), "{html}");
		assert!(html.contains("1 min ago"), "{html}");
		assert!(html.contains("Audit log"), "{html}");
		let empty = panel(&[], now, None).into_string();
		assert!(empty.contains("Nothing yet"), "{empty}");
	}

	#[test]
	fn the_footer_leads_to_the_log_or_else_to_deployments() {
		let (team, project) = (TeamId(1), ProjectId(2));
		assert_eq!(
			footer_link(team, project, true, None).map(|l| l.0),
			Some("Audit log")
		);
		assert_eq!(
			footer_link(team, project, false, Some(ApplicationId(5))),
			Some((
				"All deployments",
				"/teams/1/projects/2/apps/5/deployments".to_owned()
			))
		);
		assert_eq!(footer_link(team, project, false, None), None);
	}
}
