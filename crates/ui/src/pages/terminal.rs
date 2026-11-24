//! The Terminal tab's body, shared by applications, databases and stack services: a card with
//! the terminal in it, or a sentence on why there is none. The page around it is the caller's.

use maud::{Markup, html};

use crate::components::{Terminal, card, card_content, card_section, empty_state, terminal};
use crate::extract::TeamCtx;
use podkit_core::auth::permission::{Action, Resource};
use podkit_core::ids::ProjectId;
use podkit_core::state::AppState;
use podkit_core::terminal::service::{self, Subject};

/// Whether the person may open terminals at all; the tab is shown (and its page opens) only
/// for them. The server checks again when the socket opens.
pub fn allowed(ctx: &TeamCtx) -> bool {
	ctx.perms.can(Action::Create, Resource::Terminal)
}

/// The card. `running` is whether there is a container to open a shell in; `what` names the
/// kind ("application") for the sentence when there is not.
pub async fn panel(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	subject: &Subject,
	what: &str,
	name: &str,
	running: bool,
) -> Markup {
	let content = if running {
		let recorded = service::recording(&s.db, ctx.user.id, ctx.team.id)
			.await
			.unwrap_or(false);
		let socket = format!(
			"/ws/terminal/{}?team={}&project={}",
			subject.slug(),
			ctx.team.id,
			project
		);
		let label = format!("Terminal in {name}");
		terminal(&Terminal {
			label: &label,
			socket: &socket,
			recorded,
		})
	} else {
		empty_state(
			"terminal",
			&format!("This {what} isn't running."),
			Some("There is no container to open a shell in. Start it and come back."),
			None,
		)
	};
	card(&html! {
		(card_section("Terminal", Some("A shell inside the running container. What you type there is not recorded."), None))
		(card_content(&content))
	})
}

#[cfg(test)]
mod tests {
	use axum::http::StatusCode;
	use serde_json::json;

	use crate::testing::{assert_contains, assert_not_contains, get_html, page_main, ui_login};
	use crate::tests::{self, PgPool, app, call, db_test, user_with_team};

	struct Page {
		app: axum::Router,
		db: PgPool,
		cookie: String,
		token: String,
		team: i64,
		project: i64,
	}

	/// alice owns a team with a project, an application (5002, running in `ctr-app`) and a
	/// database (6001, running in `ctr-db`); bob is a plain member.
	async fn page(db: PgPool) -> Page {
		let app = app(db.clone()).await;
		let (token, team) = user_with_team(&app, "alice").await;
		let (_, project) = call(
			&app,
			"POST",
			&format!("/api/v1/teams/{team}/projects"),
			Some(&token),
			Some(json!({ "name": "Web", "slug": "web" })),
		)
		.await;
		let project = project["id"].as_i64().unwrap();
		tests::seed_app_and_database(&db, team, project).await;
		tests::dbs::set_state(&db, 6001, "running", Some("ctr-db")).await;
		tests::insert_deployment(&db, 5001, 9001, "running", Some("img"), Some("ctr-app")).await;
		let cookie = ui_login(&app, "alice").await;
		Page {
			app,
			db,
			cookie,
			token,
			team,
			project,
		}
	}

	impl Page {
		fn app_tab(&self) -> String {
			format!(
				"/teams/{}/projects/{}/apps/5001/terminal",
				self.team, self.project
			)
		}

		async fn get(&self, uri: &str, cookie: &str) -> (StatusCode, String) {
			let (s, _, body) = get_html(&self.app, uri, Some(cookie)).await;
			(s, body)
		}

		async fn bob(&self) -> String {
			user_with_team(&self.app, "bob").await;
			tests::notifications_ui::add_member(&self.db, self.team, "bob").await;
			ui_login(&self.app, "bob").await
		}
	}

	#[db_test(migrator = "podkit_core::db::MIGRATOR")]
	async fn an_application_tab_opens_the_terminal_at_its_socket(db: PgPool) {
		let p = page(db).await;
		let (s, body) = p.get(&p.app_tab(), &p.cookie).await;
		assert_eq!(s, StatusCode::OK);
		let main = page_main(&body);
		assert_contains(
			main,
			&format!(
				"data-socket=\"/ws/terminal/app-5001?team={}&amp;project={}\"",
				p.team, p.project
			),
		);
		assert_contains(main, "Terminal in Storefront");
		assert_contains(&body, "/static/xterm.js?v=");
		assert_contains(&body, "/static/terminal.js?v=");
		// A tab among the others, and the page's only h1.
		assert_contains(&body, "/apps/5001/terminal\"");
		assert_eq!(body.matches("<h1").count(), 1, "one h1 per page");
		// Nothing third-party.
		assert_not_contains(&body, "https://");
	}

	#[db_test(migrator = "podkit_core::db::MIGRATOR")]
	async fn other_pages_do_not_load_the_terminal(db: PgPool) {
		let p = page(db).await;
		let logs = format!("/teams/{}/projects/{}/apps/5001/logs", p.team, p.project);
		let (s, body) = p.get(&logs, &p.cookie).await;
		assert_eq!(s, StatusCode::OK);
		assert_not_contains(&body, "xterm");
		assert_not_contains(&body, "terminal.js");
	}

	#[db_test(migrator = "podkit_core::db::MIGRATOR")]
	async fn a_stopped_application_says_so_and_loads_nothing(db: PgPool) {
		let p = page(db).await;
		tests::set_deployment_status(&p.db, 9001, "stopped").await;
		let (s, body) = p.get(&p.app_tab(), &p.cookie).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(page_main(&body), "isn't running");
		assert_not_contains(&body, "data-terminal");
		assert_not_contains(&body, "xterm");
	}

	#[db_test(migrator = "podkit_core::db::MIGRATOR")]
	async fn a_member_without_the_permission_has_no_tab_and_a_403(db: PgPool) {
		let p = page(db).await;
		let bob = p.bob().await;
		let overview = format!("/teams/{}/projects/{}/apps/5001", p.team, p.project);
		let (s, body) = p.get(&overview, &bob).await;
		assert_eq!(s, StatusCode::OK);
		assert_not_contains(&body, "/apps/5001/terminal");
		let (s, body) = p.get(&p.app_tab(), &bob).await;
		assert_eq!(s, StatusCode::FORBIDDEN);
		assert_not_contains(&body, "data-socket");
	}

	#[db_test(migrator = "podkit_core::db::MIGRATOR")]
	async fn another_teams_member_gets_a_404(db: PgPool) {
		let p = page(db).await;
		user_with_team(&p.app, "carol").await;
		let carol = ui_login(&p.app, "carol").await;
		let (s, body) = p.get(&p.app_tab(), &carol).await;
		assert_eq!(s, StatusCode::NOT_FOUND);
		assert_not_contains(&body, "data-socket");
	}

	#[db_test(migrator = "podkit_core::db::MIGRATOR")]
	async fn a_database_has_a_terminal_tab_too(db: PgPool) {
		let p = page(db).await;
		let uri = format!(
			"/teams/{}/projects/{}/databases/6001/terminal",
			p.team, p.project
		);
		let (s, body) = p.get(&uri, &p.cookie).await;
		assert_eq!(s, StatusCode::OK);
		assert_contains(
			page_main(&body),
			&format!(
				"data-socket=\"/ws/terminal/database-6001?team={}&amp;project={}\"",
				p.team, p.project
			),
		);
		tests::dbs::set_state(&p.db, 6001, "stopped", None).await;
		let (_, body) = p.get(&uri, &p.cookie).await;
		assert_not_contains(&body, "data-terminal");
	}

	#[db_test(migrator = "podkit_core::db::MIGRATOR")]
	async fn the_page_says_when_the_team_records_sessions(db: PgPool) {
		let p = page(db).await;
		let (_, body) = p.get(&p.app_tab(), &p.cookie).await;
		assert_not_contains(&body, "records what its terminals print");
		let token = p.token.clone();
		let (s, _) = call(
			&p.app,
			"PUT",
			&format!("/api/v1/teams/{}/terminal/recording", p.team),
			Some(&token),
			Some(json!({ "enabled": true })),
		)
		.await;
		assert_eq!(s, StatusCode::OK);
		let (_, body) = p.get(&p.app_tab(), &p.cookie).await;
		assert_contains(&body, "records what its terminals print");
	}
}
