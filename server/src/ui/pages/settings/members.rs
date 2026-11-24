//! Members page: who is in the team and with which role, adding an existing
//! user by email, inviting an address by email (with the open invites, resend and
//! revoke), changing a role, and the roles a team can assign.

use std::collections::HashMap;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use datastar::axum::ReadSignals;
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::json;

use crate::auth::permission::{Action, Resource};
use crate::error::AppResult;
use crate::ids::{RoleId, TeamId, TeamInviteId, UserId};
use crate::members::invites::{self, Invite, Issued};
use crate::members::service::{self, MemberDetail, Role};
use crate::state::AppState;
use crate::throttle;
use crate::ui::components::{
	BUTTON, BUTTON_OUTLINE, BannerKind, INPUT, LABEL, SELECT, banner, card, card_content,
	card_heading, copy_field, icon, user_avatar,
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
		.route(
			"/teams/{team_id}/members/unlock",
			axum::routing::post(unlock),
		)
		.route(
			"/teams/{team_id}/members/invites",
			axum::routing::post(invite),
		)
		.route(
			"/teams/{team_id}/members/invites/resend",
			axum::routing::post(resend),
		)
		.route(
			"/teams/{team_id}/members/invites/revoke",
			axum::routing::post(revoke),
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
	/// Members locked out of signing in, with the seconds left. Empty for someone who
	/// may not read memberships (they could not see the page anyway).
	locked: HashMap<UserId, u64>,
	/// Open invites, expired ones included.
	invites: Vec<Invite>,
}

async fn load(s: &AppState, ctx: &TeamCtx) -> AppResult<Data> {
	let members = service::list(&s.db, ctx.user.id, ctx.team.id).await?;
	let roles = if ctx.perms.can(Action::Read, Resource::Role) {
		service::roles(&s.db, ctx.user.id, ctx.team.id).await?
	} else {
		Vec::new()
	};
	let locked = service::locked(&s.db, ctx.user.id, ctx.team.id).await?;
	let invites = invites::list(&s.db, ctx.user.id, ctx.team.id).await?;
	Ok(Data {
		members,
		roles,
		locked,
		invites,
	})
}

/// The member rows; patched in place after an add or a role change. Someone
/// with `update membership` gets a role picker per row, others plain text.
fn member_list(ctx: &TeamCtx, data: &Data) -> Markup {
	let can_update = ctx.perms.can(Action::Update, Resource::Membership) && !data.roles.is_empty();
	let post = format!(
		"$target = el.dataset.user; $role = el.value; @post('{}/role')",
		base(ctx.team.id)
	);
	let can_unlock = ctx.perms.can(Action::Update, Resource::Membership);
	let unlock = format!(
		"$target = el.dataset.user; @post('{}/unlock')",
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
						th scope="col" { "Sign-in" }
					}
				}
				tbody {
					@for m in &data.members {
						tr {
							th scope="row" {
								div class="ui-who" {
									(user_avatar(&m.display_name, m.user_id, m.avatar.as_deref()))
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
							td data-label="Sign-in" {
								@if let Some(left) = data.locked.get(&m.user_id) {
									span class="ui-tag ui-tag-warning" { "Locked, " (throttle::describe(*left)) " left" }
									@if can_unlock {
										" "
										button type="button" class=(BUTTON_OUTLINE) data-user=(m.user_id.0)
											"data-on:click"=(unlock) "data-indicator:_busy" "data-attr:disabled"="$_busy" {
											"Unlock"
										}
									}
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

/// The invite form. The result of the last invite (a link to pass on when no email went out)
/// is patched into `#invite-result` below it.
fn invite_card(ctx: &TeamCtx, data: &Data) -> Markup {
	let post = format!(
		"evt.preventDefault(); @post('{}/invites')",
		base(ctx.team.id)
	);
	card(&html! {
		(card_heading("Invite by email", Some("For someone without a podkit account, or who should join with one. The link lets them sign up even when sign-up is closed, works once and expires in 7 days.")))
		(card_content(&html! {
			form class="ui-field-row ui-field-row-inv" "data-on:submit"=(post) {
				div class="ui-field-group" {
					label class=(LABEL) for="invite-email" { "Email" }
					input id="invite-email" class=(INPUT) type="email" required maxlength="100" placeholder="name@example.com" "data-bind:invemail";
				}
				div class="ui-field-group" {
					label class=(LABEL) for="invite-role" { "Role" }
					div class="ui-select-wrap" {
						select id="invite-role" class=(SELECT) "data-bind:invrole" {
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
					span { "Send invite" }
				}
			}
			(invite_result(None))
		}))
	})
}

/// What came of the last invite: nothing, or the link and why it was not emailed. The link is
/// shown this once; a resend makes a new one.
fn invite_result(issued: Option<(&Issued, String)>) -> Markup {
	html! {
		div id="invite-result" {
			@if let Some((issued, link)) = issued {
				@if let Some(note) = &issued.note {
					@let lead = format!("Invite for {} made.", issued.invite.email);
					(banner(BannerKind::Warning, Some(lead.as_str()), note, None))
				}
				(copy_field("invite-link", "Invite link", &link))
				p class="ui-hint" { "Anyone with this link can join as " (issued.invite.email) ". It is shown only now." }
			}
		}
	}
}

/// The open invites, with resend and revoke for those who may.
fn invite_list(ctx: &TeamCtx, data: &Data) -> Markup {
	let can_send = ctx.perms.can(Action::Create, Resource::Membership);
	let can_revoke = ctx.perms.can(Action::Delete, Resource::Membership);
	let resend = format!(
		"$invite = el.dataset.invite; @post('{}/invites/resend')",
		base(ctx.team.id)
	);
	let revoke = format!(
		"$invite = el.dataset.invite; @post('{}/invites/revoke')",
		base(ctx.team.id)
	);
	let now = time::OffsetDateTime::now_utc();
	html! {
		div id="invite-list" {
			@if !data.invites.is_empty() {
				(card(&html! {
					div class="ui-card-h" {
						div {
							h2 class="ui-card-title" { "Pending invites" span class="ui-count" { (data.invites.len()) } }
							p class="ui-card-sub" {
								"Sent and not accepted yet."
								@if can_send { " Resending makes a new link; the old one stops working." }
							}
						}
					}
					table class="ui-table" {
						caption class="ui-visually-hidden" { "Pending invites" }
						thead {
							tr {
								th scope="col" { "Email" }
								th scope="col" { "Role" }
								th scope="col" { "Expires" }
								th scope="col" { span class="ui-visually-hidden" { "Actions" } }
							}
						}
						tbody {
							@for i in &data.invites {
								tr {
									th scope="row" {
										div class="ui-who" {
											div {
												b { (i.email) }
												@if let Some(by) = &i.invited_by { small { "Invited by " (by) } }
											}
										}
									}
									td data-label="Role" { (i.role_name) }
									td data-label="Expires" {
										@if i.expired(now) {
											span class="ui-tag ui-tag-warning" { "Expired" }
										} @else {
											(i.expires_at.date()) " UTC"
										}
									}
									td data-label="Actions" {
										@if can_send {
											button type="button" class=(BUTTON_OUTLINE) data-invite=(i.id.0)
												"data-on:click"=(resend) "data-indicator:_busy" "data-attr:disabled"="$_busy" {
												"Resend"
											}
										}
										@if can_revoke {
											" "
											button type="button" class=(BUTTON_OUTLINE) data-invite=(i.id.0)
												aria-label={ "Revoke the invite for " (i.email) }
												"data-on:click"=(revoke) "data-indicator:_busy" "data-attr:disabled"="$_busy" {
												"Revoke"
											}
										}
									}
								}
							}
						}
					}
				}))
			}
		}
	}
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
		"{{email: '', newrole: '{0}', invemail: '', invrole: '{0}', invite: '', target: '', role: '', _error: ''}}",
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
			@if can_add { (invite_card(&ctx, &data)) }
			(invite_list(&ctx, &data))
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

#[derive(Deserialize)]
struct UnlockSignals {
	target: String,
}

async fn unlock(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<UnlockSignals>,
) -> Response {
	let Ok(member) = sig.target.parse::<i64>().map(UserId) else {
		return sse::error("Pick a member.").into_response();
	};
	let result = async {
		service::unlock(&s.db, ctx.user.id, ctx.team.id, member).await?;
		load(&s, &ctx).await
	}
	.await;
	match result {
		Ok(data) => sse::patch(&member_list(&ctx, &data))
			.and(sse::signals(&json!({ "target": "", "_error": "" })))
			.and(sse::toast("Sign-in unlocked"))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize)]
struct InviteSignals {
	invemail: String,
	invrole: String,
}

/// The link as the person who invited can use it: absolute with `PUBLIC_URL`, else made
/// absolute with this request's `Origin`, which the origin guard has already matched to the
/// site. It is shown back to this same browser only; emails never use a request header.
fn shown_link(headers: &HeaderMap, link: &str) -> String {
	if !link.starts_with('/') {
		return link.to_owned();
	}
	headers
		.get(header::ORIGIN)
		.and_then(|o| o.to_str().ok())
		.filter(|o| o.starts_with("http://") || o.starts_with("https://"))
		.map_or_else(|| link.to_owned(), |o| format!("{o}{link}"))
}

/// The page's answer to a new or resent invite: the list redrawn, the link when it was not
/// emailed, and a toast.
fn issued_response(ctx: &TeamCtx, data: &Data, headers: &HeaderMap, issued: &Issued) -> Response {
	let result = issued
		.link
		.as_deref()
		.map(|link| (issued, shown_link(headers, link)));
	let toast = if issued.emailed {
		format!("Invite sent to {}", issued.invite.email)
	} else {
		"Invite made; copy the link".to_owned()
	};
	sse::patch(&invite_list(ctx, data))
		.and(sse::patch(&invite_result(
			result.as_ref().map(|(i, l)| (*i, l.clone())),
		)))
		.and(sse::signals(
			&json!({ "invemail": "", "invite": "", "_error": "" }),
		))
		.and(sse::toast(&toast))
		.into_response()
}

async fn invite(
	State(s): State<AppState>,
	ctx: TeamCtx,
	headers: HeaderMap,
	ReadSignals(sig): ReadSignals<InviteSignals>,
) -> Response {
	let Ok(role) = sig.invrole.parse::<i64>().map(RoleId) else {
		return sse::error("Pick a role.").into_response();
	};
	let result = async {
		let issued = invites::create(&s, ctx.user.id, ctx.team.id, &sig.invemail, role).await?;
		Ok::<_, crate::error::ServerError>((issued, load(&s, &ctx).await?))
	}
	.await;
	match result {
		Ok((issued, data)) => issued_response(&ctx, &data, &headers, &issued),
		Err(e) => error::action(&e),
	}
}

#[derive(Deserialize)]
struct InviteTarget {
	invite: String,
}

async fn resend(
	State(s): State<AppState>,
	ctx: TeamCtx,
	headers: HeaderMap,
	ReadSignals(sig): ReadSignals<InviteTarget>,
) -> Response {
	let Ok(id) = sig.invite.parse::<i64>().map(TeamInviteId) else {
		return sse::error("Pick an invite.").into_response();
	};
	let result = async {
		let issued = invites::resend(&s, ctx.user.id, ctx.team.id, id).await?;
		Ok::<_, crate::error::ServerError>((issued, load(&s, &ctx).await?))
	}
	.await;
	match result {
		Ok((issued, data)) => issued_response(&ctx, &data, &headers, &issued),
		Err(e) => error::action(&e),
	}
}

async fn revoke(
	State(s): State<AppState>,
	ctx: TeamCtx,
	ReadSignals(sig): ReadSignals<InviteTarget>,
) -> Response {
	let Ok(id) = sig.invite.parse::<i64>().map(TeamInviteId) else {
		return sse::error("Pick an invite.").into_response();
	};
	let result = async {
		invites::revoke(&s.db, ctx.user.id, ctx.team.id, id).await?;
		load(&s, &ctx).await
	}
	.await;
	match result {
		Ok(data) => sse::patch(&invite_list(&ctx, &data))
			.and(sse::patch(&invite_result(None)))
			.and(sse::signals(&json!({ "invite": "", "_error": "" })))
			.and(sse::toast("Invite revoked"))
			.into_response(),
		Err(e) => error::action(&e),
	}
}

#[cfg(test)]
mod tests {
	use axum::http::StatusCode;
	use serde_json::json;

	use crate::tests::{
		PgPool, app, call, count, db_test, execute, only_id, team_role, user_id, user_with_team,
	};
	use crate::ui::testing::{
		assert_contains, assert_not_contains, get_html, page_main, post_signals, ui_login,
		ui_session,
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

	#[db_test]
	async fn inviting_by_email_shows_the_link_lists_and_revokes(db: PgPool) {
		let app = app(db.clone()).await;
		let (alice, bob, team) = team_with_member(&app).await;
		let page = format!("/teams/{team}/members");

		let (_, _, body) = get_html(&app, &page, Some(&alice)).await;
		assert_contains(&body, "Invite by email");

		let (s, _, body) = post_signals(
			&app,
			&format!("{page}/invites"),
			Some(&alice),
			&json!({ "invemail": "newbie@example.com", "invrole": MEMBER.to_string() }),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(&body, "invite-list");
		assert_contains(&body, "newbie@example.com");
		// No PUBLIC_URL in tests: not emailed, the link is shown once, made absolute with the
		// browser's own origin.
		assert_contains(&body, "Not emailed");
		assert_contains(&body, "http://podkit.test/invite/");
		assert_eq!(count(&db, "team_invites").await, 1);

		// The page lists it without any link.
		let (_, _, body) = get_html(&app, &page, Some(&alice)).await;
		assert_contains(&body, "Pending invites");
		assert_contains(&body, "newbie@example.com");
		assert_not_contains(&body, "/invite/");
		assert_contains(&body, "Revoke");

		// A plain member sees the list, no form and no buttons, and a forged post fails.
		let (_, _, body) = get_html(&app, &page, Some(&bob)).await;
		let main = page_main(&body);
		assert_contains(main, "newbie@example.com");
		assert_not_contains(main, "Invite by email");
		assert_not_contains(main, "Revoke");
		assert_not_contains(main, "Resend");
		let id = only_id(&db, "team_invites").await;
		let (_, _, body) = post_signals(
			&app,
			&format!("{page}/invites/revoke"),
			Some(&bob),
			&json!({ "invite": id.to_string() }),
		)
		.await;
		assert_contains(&body, "You don't have permission to do that.");
		let (_, _, body) = post_signals(
			&app,
			&format!("{page}/invites"),
			Some(&bob),
			&json!({ "invemail": "x@example.com", "invrole": MEMBER.to_string() }),
		)
		.await;
		assert_contains(&body, "You don't have permission to do that.");
		assert_eq!(count(&db, "team_invites").await, 1);

		let (_, _, body) = post_signals(
			&app,
			&format!("{page}/invites/revoke"),
			Some(&alice),
			&json!({ "invite": id.to_string() }),
		)
		.await;
		assert_contains(&body, "Invite revoked");
		assert_eq!(count(&db, "team_invites").await, 0);
	}

	#[db_test]
	async fn nobody_hands_out_or_changes_more_than_their_own_role(db: PgPool) {
		let app = app(db.clone()).await;
		let (alice, team) = ui_session(&app, "alice").await;
		register(&app, "bob").await;
		register(&app, "carol").await;
		// A team role that manages memberships and reads, nothing else.
		crate::tests::team_role(&db, 901, team, "Recruiter").await;
		execute(
			&db,
			"INSERT INTO role_permissions (role_id, permission_id)
			SELECT 901, id FROM permissions WHERE resource = 'membership' OR action = 'read'",
		)
		.await;
		let url = format!("/teams/{team}/members");
		let (_, _, body) = post_signals(
			&app,
			&url,
			Some(&alice),
			&json!({ "email": "bob@example.com", "newrole": "901" }),
		)
		.await;
		assert_contains(&body, "Member added");
		let bob = ui_login(&app, "bob").await;

		// Adding with a wider role is refused; with one inside bob's own it works.
		let (_, _, body) = post_signals(
			&app,
			&url,
			Some(&bob),
			&json!({ "email": "carol@example.com", "newrole": OWNER.to_string() }),
		)
		.await;
		assert_contains(&body, "more than yours");
		let (_, _, body) = post_signals(
			&app,
			&url,
			Some(&bob),
			&json!({ "email": "carol@example.com", "newrole": MEMBER.to_string() }),
		)
		.await;
		assert_contains(&body, "Member added");

		// Raising carol past bob, or touching alice (who can do more than bob), is refused.
		let carol = user_id(&db, "carol").await;
		let alice_id = user_id(&db, "alice").await;
		let role_url = format!("{url}/role");
		for (target, role, refused) in [
			(carol, OWNER, "more than yours"),
			(alice_id, MEMBER, "can do more than you"),
		] {
			let (_, _, body) = post_signals(
				&app,
				&role_url,
				Some(&bob),
				&json!({ "target": target.to_string(), "role": role.to_string() }),
			)
			.await;
			assert_contains(&body, refused);
		}
		assert_eq!(role_of(&db, team, "carol").await, MEMBER);
		assert_eq!(role_of(&db, team, "alice").await, OWNER);

		let (_, _, body) = post_signals(
			&app,
			&role_url,
			Some(&bob),
			&json!({ "target": carol.to_string(), "role": "901" }),
		)
		.await;
		assert_contains(&body, "Role updated");
		assert_eq!(role_of(&db, team, "carol").await, 901);
	}
}
