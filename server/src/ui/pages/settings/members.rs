//! Members page: who is in the team and with which role, adding an existing
//! user by email, changing a role, and the roles a team can assign.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use crate::auth::permission::{Action, Resource};
use crate::error::AppResult;
use crate::ids::{RoleId, TeamId, UserId};
use crate::members::service::{self, MemberDetail, Role};
use crate::state::AppState;
use crate::ui::components::{
	BUTTON, INPUT, LABEL, SELECT, avatar, card, card_content, card_heading, icon,
};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::{app_page, form_error};
use crate::ui::{error, sse};

use super::frame::{settings_body, settings_crumbs};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/members", get(page).post(add))
		.route(
			"/teams/{team_id}/members/role",
			axum::routing::post(change_role),
		)
}

fn base(team: TeamId) -> String {
	format!("/teams/{team}/members")
}

/// Members and the roles on offer, as the page needs them. The roles list is
/// empty for someone who may not read roles.
struct Data {
	members: Vec<MemberDetail>,
	roles: Vec<Role>,
}

async fn load(s: &AppState, ctx: &TeamCtx) -> AppResult<Data> {
	let members = service::list(&s.db, ctx.user.id, ctx.team.id).await?;
	let roles = if ctx.perms.can(Action::Read, Resource::Role) {
		service::roles(&s.db, ctx.user.id, ctx.team.id).await?
	} else {
		Vec::new()
	};
	Ok(Data { members, roles })
}

/// The member rows; patched in place after an add or a role change. Someone
/// with `update membership` gets a role picker per row, others plain text.
fn member_list(ctx: &TeamCtx, data: &Data) -> Markup {
	let can_update = ctx.perms.can(Action::Update, Resource::Membership) && !data.roles.is_empty();
	let post = format!(
		"$target = el.dataset.user; $role = el.value; @post('{}/role')",
		base(ctx.team.id)
	);
	html! {
		div id="member-list" {
			table class="ui-table" {
				caption class="ui-visually-hidden" { "Team members" }
				thead {
					tr {
						th scope="col" { "Member" }
						th scope="col" { "Role" }
					}
				}
				tbody {
					@for m in &data.members {
						tr {
							th scope="row" {
								div class="ui-who" {
									(avatar(&m.display_name))
									div {
										b { (m.display_name) }
										small { (m.email) }
									}
								}
							}
							td data-label="Role" {
								@if can_update {
									div class="ui-select-wrap ui-select-sm" {
										select class=(SELECT) aria-label={ "Role of " (m.display_name) }
											data-user=(m.user_id.0) "data-on:change"=(post) {
											button { selectedcontent {} }
											@for r in &data.roles {
												option value=(r.id.0) selected[r.id == m.role_id] { (r.name) }
											}
										}
										(icon("chevron-down"))
									}
								} @else {
									span class="ui-role-fixed" { (m.role_name) }
								}
							}
						}
					}
				}
			}
		}
	}
}

fn add_card(ctx: &TeamCtx, data: &Data) -> Markup {
	let post = format!("evt.preventDefault(); @post('{}')", base(ctx.team.id));
	card(&html! {
		(card_heading("Add a member", Some("The person needs a podkit account already. Use the email they signed up with.")))
		(card_content(&html! {
			form class="ui-field-row ui-field-row-inv" "data-on:submit"=(post) {
				div class="ui-field-group" {
					label class=(LABEL) for="member-email" { "Email" }
					input id="member-email" class=(INPUT) type="email" required placeholder="name@example.com" "data-bind:email";
				}
				div class="ui-field-group" {
					label class=(LABEL) for="member-role" { "Role" }
					div class="ui-select-wrap" {
						select id="member-role" class=(SELECT) "data-bind:newrole" {
							button { selectedcontent {} }
							@for r in &data.roles {
								option value=(r.id.0) { (r.name) }
							}
						}
						(icon("chevron-down"))
					}
				}
				button type="submit" class=(BUTTON) "data-indicator:_busy" "data-attr:disabled"="$_busy" {
					(icon("plus"))
					span { "Add member" }
				}
			}
		}))
	})
}

fn roles_card(data: &Data) -> Markup {
	card(&html! {
		(card_heading("Roles", Some("What each role is called and who owns it. Roles are read-only here.")))
		table class="ui-table" {
			caption class="ui-visually-hidden" { "Roles" }
			thead {
				tr {
					th scope="col" { "Role" }
					th scope="col" { "Kind" }
				}
			}
			tbody {
				@for r in &data.roles {
					tr {
						th scope="row" { (r.name) }
						td data-label="Kind" {
							@if r.is_builtin() { "Built-in" } @else { "This team" }
						}
					}
				}
			}
		}
	})
}

/// The role a new member starts with: `Member` when there is one.
fn default_role(roles: &[Role]) -> String {
	roles
		.iter()
		.find(|r| r.name == "Member")
		.or_else(|| roles.first())
		.map(|r| r.id.0.to_string())
		.unwrap_or_default()
}

async fn page(State(s): State<AppState>, ctx: TeamCtx) -> Result<Markup, Response> {
	let data = load(&s, &ctx).await.map_err(|e| error::page(&ctx, &e))?;
	let can_add = ctx.perms.can(Action::Create, Resource::Membership) && !data.roles.is_empty();

	let signals = format!(
		"{{email: '', newrole: '{}', target: '', role: '', _error: ''}}",
		default_role(&data.roles)
	);
	let body = html! {
		div class="ui-settings-members" data-signals=(signals) {
			(card(&html! {
				div class="ui-card-h" {
					div {
						h2 class="ui-card-title" { "Members" span class="ui-count" { (data.members.len()) } }
						p class="ui-card-sub" { "Everyone who can work in this team." }
					}
				}
				(member_list(&ctx, &data))
				div class="ui-card-b" { (form_error()) }
			}))
			@if can_add { (add_card(&ctx, &data)) }
			@if !data.roles.is_empty() { (roles_card(&data)) }
		}
	};
	Ok(app_page(
		&ctx,
		"Members",
		&settings_crumbs(&ctx, "Members"),
		&settings_body(&ctx, &body),
	))
}

#[derive(Deserialize)]
struct AddSignals {
	email: String,
	newrole: String,
}

async fn add(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<AddSignals>,
) -> Response {
	let Ok(role) = sig.newrole.parse::<i64>().map(RoleId) else {
		return sse::error("Pick a role.").into_response();
	};
	let result = async {
		service::add(&s.db, ctx.user.id, ctx.team.id, &sig.email, role).await?;
		load(&s, &ctx).await
	}
	.await;
	match result {
		Ok(data) => sse::patch(&member_list(&ctx, &data))
			.and(sse::signals(&json!({ "email": "", "_error": "" })))
			.and(sse::toast("Member added"))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize)]
struct RoleSignals {
	target: String,
	role: String,
}

async fn change_role(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<RoleSignals>,
) -> Response {
	let (Ok(member), Ok(role)) = (
		sig.target.parse::<i64>().map(UserId),
		sig.role.parse::<i64>().map(RoleId),
	) else {
		return sse::error("Pick a member and a role.").into_response();
	};
	let changed = service::update_role(&s.db, ctx.user.id, ctx.team.id, member, role).await;
	// The picker already shows the new role; redraw from the database so a
	// refused change snaps back.
	let data = match load(&s, &ctx).await {
		Ok(data) => data,
		Err(e) => return error::action(&e),
	};
	let redraw = sse::patch(&member_list(&ctx, &data));
	match changed {
		Ok(()) => redraw
			.and(sse::signals(
				&json!({ "target": "", "role": "", "_error": "" }),
			))
			.and(sse::toast("Role updated"))
			.into_response(),
		Err(e) => redraw.and(sse::error(&error::message(&e))).into_response(),
	}
}

#[cfg(test)]
mod tests {
	use axum::http::StatusCode;
	use serde_json::json;

	use crate::tests::{PgPool, app, call, count, db_test, team_role, user_id, user_with_team};
	use crate::ui::testing::{
		assert_contains, assert_not_contains, get_html, post_signals, ui_login, ui_session,
	};

	const OWNER: i64 = 1;
	const MEMBER: i64 = 2;

	async fn register(app: &axum::Router, name: &str) {
		let creds = json!({
			"name": name, "email": format!("{name}@example.com"), "password": "correct horse battery",
		});
		let (s, _) = call(app, "POST", "/api/v1/auth/register", None, Some(creds)).await;
		assert!(s.is_success());
	}

	use crate::tests::member_role as role_of;

	/// alice owns a team; bob joins it as a plain member. Returns
	/// (alice cookie, bob cookie, team).
	async fn team_with_member(app: &axum::Router) -> (String, String, i64) {
		let (alice, team) = ui_session(app, "alice").await;
		register(app, "bob").await;
		let (s, _, _) = post_signals(
			app,
			&format!("/teams/{team}/members"),
			Some(&alice),
			&json!({ "email": "bob@example.com", "newrole": MEMBER.to_string() }),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		(alice, ui_login(app, "bob").await, team)
	}

	#[db_test]
	async fn members_page_lists_members_roles_and_the_owners_controls(db: PgPool) {
		let app = app(db.clone()).await;
		let (alice, bob, team) = team_with_member(&app).await;
		let page = format!("/teams/{team}/members");

		let (s, _, body) = get_html(&app, &page, Some(&alice)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "alice@example.com");
		assert_contains(&body, "bob@example.com");
		assert_contains(&body, "Add a member");
		assert_contains(&body, "<select");
		assert_contains(&body, "Built-in");

		// A plain member sees names and roles but no controls.
		let (s, _, body) = get_html(&app, &page, Some(&bob)).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "bob@example.com");
		assert_not_contains(&body, "<select");
		assert_not_contains(&body, "Add a member");
		assert_contains(&body, "Roles");
	}

	#[db_test]
	async fn adding_a_member_by_email(db: PgPool) {
		let app = app(db.clone()).await;
		let (alice, team) = ui_session(&app, "alice").await;
		register(&app, "carol").await;
		let url = format!("/teams/{team}/members");

		let (_, _, body) = post_signals(
			&app,
			&url,
			Some(&alice),
			&json!({ "email": "carol@example.com", "newrole": MEMBER.to_string() }),
		)
		.await;
		assert_contains(&body, "Member added");
		assert_contains(&body, "member-list");
		assert_eq!(role_of(&db, team, "carol").await, MEMBER);

		let (_, _, body) = post_signals(
			&app,
			&url,
			Some(&alice),
			&json!({ "email": "carol@example.com", "newrole": MEMBER.to_string() }),
		)
		.await;
		assert_contains(&body, "user is already a member of this team");

		let (_, _, body) = post_signals(
			&app,
			&url,
			Some(&alice),
			&json!({ "email": "nobody@example.com", "newrole": MEMBER.to_string() }),
		)
		.await;
		assert_contains(&body, "no user with that email");
		assert_eq!(count(&db, "team_members").await, 2);
	}

	#[db_test]
	async fn changing_a_role_works_and_a_refused_role_shows_an_inline_error(db: PgPool) {
		let app = app(db.clone()).await;
		let (alice, _, team) = team_with_member(&app).await;
		let (_, other_team) = user_with_team(&app, "zed").await;
		team_role(&db, 900, other_team, "Foreign").await;
		let bob = user_id(&db, "bob").await;
		let url = format!("/teams/{team}/members/role");

		let (_, _, body) = post_signals(
			&app,
			&url,
			Some(&alice),
			&json!({ "target": bob.to_string(), "role": OWNER.to_string() }),
		)
		.await;
		assert_contains(&body, "Role updated");
		assert_eq!(role_of(&db, team, "bob").await, OWNER);

		for refused in ["900", "424242"] {
			let (s, _, body) = post_signals(
				&app,
				&url,
				Some(&alice),
				&json!({ "target": bob.to_string(), "role": refused }),
			)
			.await;
			assert_eq!(s, StatusCode::OK);
			assert_contains(&body, "that role does not belong to this team");
			assert_contains(&body, "member-list");
			assert_eq!(role_of(&db, team, "bob").await, OWNER, "role {refused}");
		}

		// Someone who is not in this team can't be given a role in it.
		let (_, _, body) = post_signals(
			&app,
			&url,
			Some(&alice),
			&json!({ "target": "7", "role": MEMBER.to_string() }),
		)
		.await;
		assert_contains(&body, "member not found");
	}

	#[db_test]
	async fn a_member_without_update_permission_is_refused_a_forged_post(db: PgPool) {
		let app = app(db.clone()).await;
		let (_, bob, team) = team_with_member(&app).await;
		register(&app, "carol").await;
		let bob_id = user_id(&db, "bob").await;

		let (_, _, body) = post_signals(
			&app,
			&format!("/teams/{team}/members/role"),
			Some(&bob),
			&json!({ "target": bob_id.to_string(), "role": OWNER.to_string() }),
		)
		.await;
		assert_contains(&body, "You don't have permission to do that.");
		assert_eq!(role_of(&db, team, "bob").await, MEMBER);

		let (_, _, body) = post_signals(
			&app,
			&format!("/teams/{team}/members"),
			Some(&bob),
			&json!({ "email": "carol@example.com", "newrole": MEMBER.to_string() }),
		)
		.await;
		assert_contains(&body, "You don't have permission to do that.");
		assert_eq!(count(&db, "team_members").await, 2);
	}

	#[db_test]
	async fn another_teams_members_page_is_a_404_and_acts_on_nothing(db: PgPool) {
		let app = app(db.clone()).await;
		let (alice, team) = ui_session(&app, "alice").await;
		let (bob, _) = ui_session(&app, "bob").await;

		let (s, _, body) = get_html(&app, &format!("/teams/{team}/members"), Some(&bob)).await;
		assert_eq!(s, StatusCode::NOT_FOUND);
		assert_not_contains(&body, "alice@example.com");

		let (_, _, body) = post_signals(
			&app,
			&format!("/teams/{team}/members"),
			Some(&bob),
			&json!({ "email": "bob@example.com", "newrole": OWNER.to_string() }),
		)
		.await;
		assert_contains(&body, "That no longer exists.");
		assert_eq!(count(&db, "team_members").await, 2);

		let (s, _, _) = get_html(&app, &format!("/teams/{team}/members"), Some(&alice)).await;
		assert_eq!(s, StatusCode::OK);
	}

	#[db_test]
	async fn member_names_are_escaped(db: PgPool) {
		let app = app(db.clone()).await;
		let (alice, team) = ui_session(&app, "alice").await;
		let evil = "<script>alert(1)</script>";
		let creds = json!({
			"name": evil, "email": "evil@example.com", "password": "correct horse battery",
		});
		let (s, _) = call(&app, "POST", "/api/v1/auth/register", None, Some(creds)).await;
		assert!(s.is_success());
		post_signals(
			&app,
			&format!("/teams/{team}/members"),
			Some(&alice),
			&json!({ "email": "evil@example.com", "newrole": MEMBER.to_string() }),
		)
		.await;

		let (_, _, body) = get_html(&app, &format!("/teams/{team}/members"), Some(&alice)).await;
		assert_not_contains(&body, evil);
		assert_contains(&body, "&lt;script&gt;alert(1)&lt;/script&gt;");
	}
}
