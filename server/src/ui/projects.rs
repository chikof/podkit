//! Project pages: the team's project list (create, delete) and a project's
//! overview.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use super::components::{
	BUTTON, BUTTON_DANGER, BUTTON_OUTLINE, INPUT, LABEL, card, card_content, card_header,
};
use super::extract::TeamCtx;
use super::layout::{Crumb, app_page, form_error};
use super::{databases, error, sse};
use crate::auth::permission::{Action, Resource};
use crate::error::ServerError;
use crate::ids::{ProjectId, TeamId};
use crate::projects::service::{self, ProjectDetail, ProjectSummary};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/projects", get(list_page).post(create))
		.route("/teams/{team_id}/projects/delete", post(remove))
		.route("/teams/{team_id}/projects/{project_id}", get(overview))
}

fn list_url(team: TeamId) -> String {
	format!("/teams/{team}/projects")
}

fn crumbs<'a>(ctx: &TeamCtx) -> Vec<Crumb<'a>> {
	vec![("Projects", Some(list_url(ctx.team.id)))]
}

/// The rows of the project list; patched in place after create and delete.
fn project_list(ctx: &TeamCtx, projects: &[ProjectSummary]) -> Markup {
	let can_delete = ctx.perms.can(Action::Delete, Resource::Project);
	html! {
		div id="project-list" {
			@if projects.is_empty() {
				p class="text-sm text-muted-foreground" { "No projects yet." }
			} @else {
				// Not `divide-y`: encre-css 0.21.1 puts that border below each row.
				ul {
					@for p in projects {
						li class="flex flex-wrap items-center justify-between gap-3 border-t border-border py-3 text-sm first:border-t-0" {
							div class="flex min-w-0 flex-col gap-1" {
								a class="font-medium hover:underline" href={ "/teams/" (ctx.team.id.0) "/projects/" (p.project.id.0) } { (p.project.name) }
								span class="text-muted-foreground" { (p.project.slug) }
							}
							div class="flex items-center gap-4" {
								span class="text-muted-foreground" {
									(p.applications) @if p.applications == 1 { " application" } @else { " applications" }
									" · "
									(p.databases) @if p.databases == 1 { " database" } @else { " databases" }
								}
								@if can_delete {
									button type="button" class=(BUTTON_DANGER)
										data-id=(p.project.id.0) data-slug=(p.project.slug)
										"data-on:click"="$target = el.dataset.id; $targetSlug = el.dataset.slug; $confirm = ''; $_error = ''" { "Delete" }
								}
							}
						}
					}
				}
			}
		}
	}
}

fn create_card(ctx: &TeamCtx) -> Markup {
	let post = format!("evt.preventDefault(); @post('{}')", list_url(ctx.team.id));
	card(&html! {
		(card_header("New project", Some("Projects group the applications and databases that belong together.")))
		(card_content(&html! {
			form class="flex flex-col gap-4" "data-on:submit"=(post) {
				label class=(LABEL) {
					"Name"
					input class=(INPUT) type="text" required maxlength="60" "data-bind:name";
				}
				label class=(LABEL) {
					"Slug"
					input class=(INPUT) type="text" required maxlength="60" placeholder="my-project" "data-bind:slug";
				}
				(form_error())
				div {
					button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Create project" }
				}
			}
		}))
	})
}

fn delete_card(ctx: &TeamCtx) -> Markup {
	let post = format!("@post('{}/delete')", list_url(ctx.team.id));
	html! {
		div "data-show"="$target" style="display: none" {
			(card(&html! {
				(card_header("Delete project", None))
				(card_content(&html! {
					div class="flex flex-col gap-4" {
						p class="text-sm" {
							"This deletes the project and everything in it. Type "
							strong "data-text"="$targetSlug" {}
							" to confirm."
						}
						input class=(INPUT) type="text" autocomplete="off" aria-label="Project slug" "data-bind:confirm";
						(form_error())
						div class="flex gap-3" {
							button type="button" class=(BUTTON_DANGER) "data-on:click"=(post) "data-indicator:_busy" "data-attr:disabled"="$_busy" { "Delete project" }
							button type="button" class=(BUTTON_OUTLINE) "data-on:click"="$target = ''; $_error = ''" { "Cancel" }
						}
					}
				}))
			}))
		}
	}
}

async fn list_page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let projects = service::list(&s.db, ctx.user.id, ctx.team.id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let can_create = ctx.perms.can(Action::Create, Resource::Project);
	let can_delete = ctx.perms.can(Action::Delete, Resource::Project);

	let body = html! {
		div class="flex flex-col gap-6" data-signals="{name: '', slug: '', target: '', targetSlug: '', confirm: '', _error: ''}" {
			(card(&html! {
				(card_header("Projects", Some("Everything this team runs, grouped.")))
				(card_content(&project_list(&ctx, &projects)))
			}))
			@if can_delete { (delete_card(&ctx)) }
			@if can_create { (create_card(&ctx)) }
		}
	};
	Ok(app_page(&ctx, "Projects", &crumbs(&ctx), &body))
}

#[derive(Deserialize)]
struct CreateSignals {
	name: String,
	slug: String,
}

async fn create(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<CreateSignals>,
) -> Response {
	let result = async {
		let project =
			service::create(&s.db, ctx.user.id, ctx.team.id, &sig.name, &sig.slug).await?;
		let projects = service::list(&s.db, ctx.user.id, ctx.team.id).await?;
		Ok::<_, ServerError>((project, projects))
	}
	.await;
	match result {
		Ok((project, projects)) => sse::patch(&project_list(&ctx, &projects))
			.and(sse::signals(
				&json!({ "name": "", "slug": "", "_error": "" }),
			))
			.and(sse::toast(&format!("Created {}", project.name)))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize)]
struct DeleteSignals {
	target: String,
	confirm: String,
}

async fn remove(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<DeleteSignals>,
) -> Response {
	let Ok(id) = sig.target.parse::<i64>().map(ProjectId) else {
		return sse::error("Pick a project to delete.").into_response();
	};
	let result = async {
		// The slug is checked after a read so a mistyped slug never reaches
		// the delete; `remove` still authorizes the delete itself.
		let found = service::get(&s.db, ctx.user.id, ctx.team.id, id).await?;
		if sig.confirm != found.project.slug {
			return Ok(None);
		}
		let project = service::remove(&s.db, ctx.user.id, ctx.team.id, id).await?;
		let projects = service::list(&s.db, ctx.user.id, ctx.team.id).await?;
		Ok::<_, ServerError>(Some((project, projects)))
	}
	.await;
	match result {
		Ok(Some((project, projects))) => sse::patch(&project_list(&ctx, &projects))
			.and(sse::signals(
				&json!({ "target": "", "targetSlug": "", "confirm": "", "_error": "" }),
			))
			.and(sse::toast(&format!("Deleted {}", project.name)))
			.into_response(),
		Ok(None) => {
			sse::error("The slug doesn't match. Type it exactly to confirm.").into_response()
		}
		Err(e) => error::action(&e),
	}
}

fn resource_rows(title: &str, empty: &str, rows: &[service::ResourceRow]) -> Markup {
	card(&html! {
		(card_header(title, None))
		(card_content(&html! {
			@if rows.is_empty() {
				p class="text-sm text-muted-foreground" { (empty) }
			} @else {
				ul {
					@for row in rows {
						li class="flex items-baseline justify-between border-t border-border py-3 text-sm first:border-t-0" {
							strong { (row.name) }
							span class="text-muted-foreground" { (row.detail) }
						}
					}
				}
			}
		}))
	})
}

async fn overview(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project)): Path<(TeamId, ProjectId)>,
) -> Result<Markup, Response> {
	let ProjectDetail {
		project,
		applications,
		databases,
	} = service::get(&s.db, ctx.user.id, ctx.team.id, project)
		.await
		.map_err(|e| error::page(&ctx, &e))?;

	let mut crumbs = crumbs(&ctx);
	crumbs.push((project.name.as_str(), None));
	let body = html! {
		div {
			h1 class="text-xl font-semibold" { (project.name) }
			p class="text-sm text-muted-foreground" { (project.slug) }
		}
		(resource_rows("Applications", "No applications yet.", &applications))
		(databases::project_card(&ctx, project.id, &databases))
	};
	Ok(app_page(&ctx, &project.name, &crumbs, &body))
}

#[cfg(test)]
mod tests {
	use crate::tests::{
		PgPool, app, call, count, db_test, project_id, seed_app_and_database, user_with_team,
	};
	use crate::ui::testing::{
		assert_contains, assert_not_contains, get_html, post_signals, ui_login, ui_session,
	};
	use axum::http::StatusCode;
	use serde_json::json;

	async fn project_count(db: &PgPool) -> i64 {
		count(db, "projects").await
	}

	#[db_test]
	async fn the_list_shows_projects_with_counts_and_the_forms(db: PgPool) {
		let app = app(db.clone()).await;
		let (cookie, team) = ui_session(&app, "alice").await;
		let list = format!("/teams/{team}/projects");

		let (s, _, body) = get_html(&app, &list, Some(&cookie)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "No projects yet.");
		assert_contains(&body, "New project");
		assert_contains(&body, "<title>Projects");
		assert_contains(&body, "Backup targets");

		let (s, _, body) = post_signals(
			&app,
			&list,
			Some(&cookie),
			&json!({ "name": "Web shop", "slug": "web-shop" }),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "Created Web shop");
		assert_contains(&body, "project-list");
		assert_eq!(project_count(&db).await, 1);

		let (_, _, body) = get_html(&app, &list, Some(&cookie)).await;
		assert_contains(&body, "Web shop");
		assert_contains(&body, "web-shop");
		assert_contains(&body, "0 applications");
		assert_contains(&body, "0 databases");
	}

	#[db_test]
	async fn creating_rejects_bad_and_duplicate_input_with_a_message(db: PgPool) {
		let app = app(db.clone()).await;
		let (cookie, team) = ui_session(&app, "alice").await;
		let list = format!("/teams/{team}/projects");

		let (_, _, body) = post_signals(
			&app,
			&list,
			Some(&cookie),
			&json!({ "name": "", "slug": "x" }),
		)
		.await;
		assert_contains(&body, "Name must be");
		let (_, _, body) = post_signals(
			&app,
			&list,
			Some(&cookie),
			&json!({ "name": "X", "slug": "Not A Slug" }),
		)
		.await;
		assert_contains(&body, "Slug must be");

		let ok = json!({ "name": "One", "slug": "one" });
		post_signals(&app, &list, Some(&cookie), &ok).await;
		let (s, _, body) = post_signals(&app, &list, Some(&cookie), &ok).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "A project with that slug already exists.");
		assert_eq!(project_count(&db).await, 1);
	}

	#[db_test]
	async fn deleting_needs_the_slug_typed_exactly(db: PgPool) {
		let app = app(db.clone()).await;
		let (cookie, team) = ui_session(&app, "alice").await;
		let list = format!("/teams/{team}/projects");
		post_signals(
			&app,
			&list,
			Some(&cookie),
			&json!({ "name": "Doomed", "slug": "doomed" }),
		)
		.await;
		let id = project_id(&db, "doomed").await.to_string();
		let delete = format!("{list}/delete");

		let (_, _, body) = post_signals(
			&app,
			&delete,
			Some(&cookie),
			&json!({ "target": id, "confirm": "nope" }),
		)
		.await;
		assert_contains(&body, "doesn't match");
		assert_eq!(project_count(&db).await, 1);

		let (s, _, body) = post_signals(
			&app,
			&delete,
			Some(&cookie),
			&json!({ "target": id, "confirm": "doomed" }),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "Deleted Doomed");
		assert_eq!(project_count(&db).await, 0);

		let (_, _, body) = post_signals(
			&app,
			&delete,
			Some(&cookie),
			&json!({ "target": "", "confirm": "" }),
		)
		.await;
		assert_contains(&body, "Pick a project");
	}

	#[db_test]
	async fn project_names_are_escaped(db: PgPool) {
		let app = app(db.clone()).await;
		let (cookie, team) = ui_session(&app, "alice").await;
		let list = format!("/teams/{team}/projects");
		let name = "<script>alert(1)</script>";

		let (_, _, body) = post_signals(
			&app,
			&list,
			Some(&cookie),
			&json!({ "name": name, "slug": "xss" }),
		)
		.await;
		assert_not_contains(&body, name);
		assert_contains(&body, "&lt;script&gt;alert(1)&lt;/script&gt;");

		for uri in [
			list.clone(),
			format!("{list}/{}", project_id(&db, "xss").await),
		] {
			let (s, _, body) = get_html(&app, &uri, Some(&cookie)).await;
			assert_eq!(s, StatusCode::OK, "{uri}");
			assert_not_contains(&body, name);
			assert_contains(&body, "&lt;script&gt;alert(1)&lt;/script&gt;");
		}
	}

	#[db_test]
	async fn another_teams_pages_are_the_404_page_and_leak_nothing(db: PgPool) {
		let app = app(db.clone()).await;
		let (alice, alice_team) = ui_session(&app, "alice").await;
		let (bob, bob_team) = ui_session(&app, "bob").await;
		post_signals(
			&app,
			&format!("/teams/{alice_team}/projects"),
			Some(&alice),
			&json!({ "name": "Secret plans", "slug": "secret-plans" }),
		)
		.await;
		let secret = project_id(&db, "secret-plans").await;

		for uri in [
			format!("/teams/{alice_team}/projects"),
			format!("/teams/{alice_team}/projects/{secret}"),
			// Bob's own team, Alice's project id.
			format!("/teams/{bob_team}/projects/{secret}"),
			"/teams/999/projects".to_owned(),
			"/teams/nope/projects".to_owned(),
		] {
			let (s, _, body) = get_html(&app, &uri, Some(&bob)).await;
			assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
			assert_contains(&body, "Not found");
			assert_not_contains(&body, "Secret plans");
			assert_not_contains(&body, "secret-plans");
			assert_not_contains(&body, "alice");
		}

		let (_, _, body) = post_signals(
			&app,
			&format!("/teams/{alice_team}/projects"),
			Some(&bob),
			&json!({ "name": "Planted", "slug": "planted" }),
		)
		.await;
		assert_contains(&body, "That no longer exists.");
		let (_, _, body) = post_signals(
			&app,
			&format!("/teams/{bob_team}/projects/delete"),
			Some(&bob),
			&json!({ "target": secret.to_string(), "confirm": "secret-plans" }),
		)
		.await;
		assert_contains(&body, "That no longer exists.");
		assert_eq!(project_count(&db).await, 1);
	}

	/// Adds `name` to `team` with the built-in read-only Member role (id 2).
	async fn join_as_member(app: &axum::Router, owner_token: &str, team: i64, name: &str) {
		let (s, _) = call(
			app,
			"POST",
			&format!("/api/v1/teams/{team}/members"),
			Some(owner_token),
			Some(json!({ "email": format!("{name}@example.com"), "role_id": 2 })),
		)
		.await;
		assert!(s.is_success(), "add member: {s}");
	}

	#[db_test]
	async fn a_member_without_create_and_delete_sees_no_actions_and_forged_posts_fail(db: PgPool) {
		let app = app(db.clone()).await;
		let (owner_token, team) = user_with_team(&app, "alice").await;
		let owner = ui_login(&app, "alice").await;
		let (_, _) = user_with_team(&app, "carol").await;
		join_as_member(&app, &owner_token, team, "carol").await;
		let carol = ui_login(&app, "carol").await;
		let list = format!("/teams/{team}/projects");
		post_signals(
			&app,
			&list,
			Some(&owner),
			&json!({ "name": "Shared", "slug": "shared" }),
		)
		.await;

		let (s, _, body) = get_html(&app, &list, Some(&carol)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "Shared");
		assert_not_contains(&body, "New project");
		assert_not_contains(&body, "Create project");
		assert_not_contains(&body, "Delete");

		let (_, _, body) = post_signals(
			&app,
			&list,
			Some(&carol),
			&json!({ "name": "Forged", "slug": "forged" }),
		)
		.await;
		assert_contains(&body, "You don't have permission to do that.");
		let id = project_id(&db, "shared").await.to_string();
		let (_, _, body) = post_signals(
			&app,
			&format!("{list}/delete"),
			Some(&carol),
			&json!({ "target": id, "confirm": "shared" }),
		)
		.await;
		assert_contains(&body, "You don't have permission to do that.");
		assert_eq!(project_count(&db).await, 1);

		// The owner does get the forms.
		let (_, _, body) = get_html(&app, &list, Some(&owner)).await;
		assert_contains(&body, "New project");
		assert_contains(&body, "Delete");
	}

	#[db_test]
	async fn an_unauthenticated_action_is_sent_to_sign_in(db: PgPool) {
		let app = app(db.clone()).await;
		let (_, team) = user_with_team(&app, "alice").await;
		let (s, _, body) = post_signals(
			&app,
			&format!("/teams/{team}/projects"),
			None,
			&json!({ "name": "Anon", "slug": "anon" }),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "window.location.assign('/login')");
		assert_eq!(project_count(&db).await, 0);
	}

	#[db_test]
	async fn the_overview_lists_applications_and_databases(db: PgPool) {
		let app = app(db.clone()).await;
		let (cookie, team) = ui_session(&app, "alice").await;
		post_signals(
			&app,
			&format!("/teams/{team}/projects"),
			Some(&cookie),
			&json!({ "name": "Shop", "slug": "shop" }),
		)
		.await;
		let project = project_id(&db, "shop").await;
		seed_app_and_database(&db, team, project).await;

		let (s, _, body) = get_html(
			&app,
			&format!("/teams/{team}/projects/{project}"),
			Some(&cookie),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "Storefront");
		assert_contains(&body, "storefront");
		assert_contains(&body, "orders");
		assert_contains(&body, "PostgreSQL 17");
		assert_contains(&body, "/databases/6001");
		assert_contains(&body, "Pending");

		let (_, _, body) = get_html(&app, &format!("/teams/{team}/projects"), Some(&cookie)).await;
		assert_contains(&body, "1 application");
		assert_contains(&body, "1 database");
	}
}
