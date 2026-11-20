//! The command palette's results: `GET /teams/{team}/search?q=` answers with a Datastar
//! patch that replaces the palette's listbox.
//!
//! Everything shown is scoped to the team in the path and to what the caller may read
//! (the `search` service does both), pages and actions come from the same permission-aware
//! list as the sidebar, and every name goes through maud's escaping.
//!
//! The palette also renders this list for an empty query ([`defaults`]), so it is useful
//! before anything is typed.

use axum::Router;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::{Markup, html};
use serde::Deserialize;

use crate::auth::permission::{Action, Resource};
use crate::ids::{ApplicationId, DatabaseId, ModelId, TeamId};
use crate::search::{Hit, Kind};
use crate::state::AppState;
use crate::ui::components::icon;
use crate::ui::extract::TeamCtx;
use crate::ui::pages::{databases, models};
use crate::ui::shell::nav;
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new().route("/teams/{team_id}/search", get(search))
}

/// The id of the listbox element the results replace.
pub const LIST_ID: &str = "palette-list";

/// One thing the palette can open.
struct Entry {
	icon: &'static str,
	label: String,
	/// Where it lives ("web-shop · production"), shown quietly after the label.
	detail: Option<String>,
	href: String,
}

struct Group {
	heading: &'static str,
	entries: Vec<Entry>,
}

#[derive(Deserialize)]
struct SearchQuery {
	#[serde(default)]
	q: String,
}

async fn search(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Query(query): Query<SearchQuery>,
) -> Response {
	match crate::search::service::search(&s.db, ctx.user.id, ctx.team.id, &query.q).await {
		Ok(hits) => sse::patch(&list(&groups(&ctx, &query.q, &hits), &query.q)).into_response(),
		Err(e) => error::action(&e),
	}
}

/// The palette before anything is typed: every page and action the user may use.
pub fn defaults(ctx: &TeamCtx) -> Markup {
	list(&groups(ctx, "", &[]), "")
}

fn contains(haystack: &str, needle: &str) -> bool {
	needle.is_empty() || haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn groups(ctx: &TeamCtx, q: &str, hits: &[Hit]) -> Vec<Group> {
	let q = q.trim();
	let team = ctx.team.id;
	let mut out = Vec::new();

	for (kind, heading) in [
		(Kind::Project, "Projects"),
		(Kind::Environment, "Environments"),
		(Kind::Application, "Applications"),
		(Kind::Database, "Databases"),
		(Kind::Model, "Models"),
		(Kind::Server, "Servers"),
	] {
		let entries: Vec<Entry> = hits
			.iter()
			.filter(|h| h.kind == kind)
			.map(|h| entry(team, h))
			.collect();
		if !entries.is_empty() {
			out.push(Group { heading, entries });
		}
	}

	let pages: Vec<Entry> = nav::places(ctx)
		.into_iter()
		.chain(nav::settings_items(ctx))
		.filter(|i| contains(i.label, q))
		.map(|i| Entry {
			icon: i.icon,
			label: i.label.to_owned(),
			detail: None,
			href: i.href,
		})
		.collect();
	if !pages.is_empty() {
		out.push(Group {
			heading: "Pages",
			entries: pages,
		});
	}

	let mut actions: Vec<Entry> = Vec::new();
	if !q.is_empty() && ctx.perms.can(Action::Create, Resource::Deployment) {
		actions.extend(
			hits.iter()
				.filter(|h| h.kind == Kind::Application)
				.map(|h| Entry {
					icon: "rocket",
					label: format!("Deploy {}", h.name),
					detail: Some("Opens the application".to_owned()),
					href: entry(team, h).href,
				}),
		);
	}
	actions.extend(
		static_actions(ctx)
			.into_iter()
			.filter(|a| contains(&a.label, q)),
	);
	if !actions.is_empty() {
		out.push(Group {
			heading: "Actions",
			entries: actions,
		});
	}
	out
}

fn static_actions(ctx: &TeamCtx) -> Vec<Entry> {
	let base = format!("/teams/{}", ctx.team.id);
	let can = |action, resource| ctx.perms.can(action, resource);
	let mut out = Vec::new();
	let mut action = |icon, label: &str, href: String| {
		out.push(Entry {
			icon,
			label: label.to_owned(),
			detail: None,
			href,
		});
	};
	if can(Action::Create, Resource::Project) {
		action("plus", "Create project", format!("{base}/projects"));
	}
	if can(Action::Create, Resource::Server) {
		action("plus", "Add server", format!("{base}/servers"));
	}
	if can(Action::Create, Resource::Backup) {
		action(
			"plus",
			"Add S3 destination",
			format!("{base}/backup-targets"),
		);
	}
	if can(Action::Read, Resource::Model) {
		action(
			"search",
			"Find a model in the catalog",
			format!("{base}/models/catalog"),
		);
	}
	action("plus", "Create team", "/teams/new".to_owned());
	out
}

fn entry(team: TeamId, hit: &Hit) -> Entry {
	let place = match (&hit.project, &hit.environment) {
		(Some((_, p)), Some((_, e))) => Some(format!("{p} · {e}")),
		(Some((_, p)), None) => Some(p.clone()),
		_ => None,
	};
	let (icon_name, href) = match (hit.kind, hit.project.as_ref().map(|(id, _)| *id)) {
		(Kind::Project, _) => ("folder", format!("/teams/{team}/projects/{}", hit.id)),
		(Kind::Environment, Some(project)) => (
			"layers",
			format!("/teams/{team}/projects/{project}?environment={}", hit.id),
		),
		(Kind::Application, Some(project)) => (
			"rocket",
			crate::ui::pages::apps::app_url(team, project, ApplicationId(hit.id)),
		),
		(Kind::Database, Some(project)) => (
			"database",
			databases::database_url(team, project, DatabaseId(hit.id)),
		),
		(Kind::Model, Some(project)) => {
			("brain", models::model_url(team, project, ModelId(hit.id)))
		}
		(Kind::Server, _) => ("server", format!("/teams/{team}/servers/{}", hit.id)),
		// A resource always has its project; one that somehow does not goes to the team.
		_ => ("folder", format!("/teams/{team}/projects")),
	};
	Entry {
		icon: icon_name,
		label: hit.name.clone(),
		detail: place,
		href,
	}
}

/// The listbox. The first option is selected and named by the input's
/// `aria-activedescendant`; `podkit.js` moves the selection with the arrow keys.
fn list(groups: &[Group], query: &str) -> Markup {
	let starts: Vec<usize> = groups
		.iter()
		.scan(0, |total, g| {
			let start = *total;
			*total += g.entries.len();
			Some(start)
		})
		.collect();
	html! {
		div id=(LIST_ID) role="listbox" aria-label="Results" class="ui-palette-list" data-palette-list {
			@if groups.is_empty() {
				div role="option" aria-disabled="true" aria-selected="false" class="ui-palette-empty" {
					@if query.trim().is_empty() { "Nothing to show." } @else { "No results for “" (query.trim()) "”." }
				}
			}
			@for (group, start) in groups.iter().zip(starts) {
				div role="group" aria-label=(group.heading) {
					div class="ui-palette-heading" aria-hidden="true" { (group.heading) }
					@for (i, e) in group.entries.iter().enumerate() {
						@let n = start + i;
						a id={ "palette-opt-" (n) } href=(e.href) role="option" aria-selected=(if n == 0 { "true" } else { "false" }) tabindex="-1" class="ui-palette-option" {
							(icon(e.icon))
							span class="min-w-0 flex-1 truncate" { (e.label) }
							@if let Some(detail) = &e.detail {
								span class="ui-palette-detail truncate" { (detail) }
							}
						}
					}
				}
			}
		}
	}
}
