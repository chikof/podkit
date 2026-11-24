//! MCP server resource tests against a fake agent on the local server's socket.
//!
//! The tests of `podkit_core::mcp_servers` that need the pages: they sign in through the
//! web UI or read what it renders, so they run against the whole router. The rest are beside
//! the code, in `podkit_core::mcp_servers::tests`.

use axum::http::StatusCode;
use serde_json::json;
use sqlx::PgPool;

use crate::tests::{state, user_with_team};
use podkit_core::tests::mcp_servers::{Setup, fake_agent};

mod pages {
	use super::*;
	use crate::testing::{assert_contains, assert_not_contains, get_html, post_signals, ui_login};

	/// The shared [`Setup`] over the whole router: these tests read the pages.
	async fn setup(db: PgPool) -> Setup {
		let state = state(db.clone());
		let agent = fake_agent(&state);
		let app = crate::routes(state.clone()).await.unwrap();
		Setup::over(db, state, agent, app).await
	}

	/// What the page tests ask of the shared [`Setup`].
	trait Pages {
		async fn cookie(&self) -> String;
		fn page(&self, mcp: i64) -> String;
		async fn link_count(&self) -> i64;
		fn tab(&self) -> String;
	}

	impl Pages for Setup {
		async fn cookie(&self) -> String {
			ui_login(&self.app, "alice").await
		}

		fn page(&self, mcp: i64) -> String {
			format!("/teams/{}/projects/{}/mcp/{mcp}", self.team, self.project)
		}

		async fn link_count(&self) -> i64 {
			sqlx::query_scalar("SELECT count(*) FROM application_mcp_links")
				.fetch_one(&self.db)
				.await
				.unwrap()
		}

		fn tab(&self) -> String {
			format!(
				"/teams/{}/projects/{}/apps/{}/mcp",
				self.team, self.project, self.consumer
			)
		}
	}

	#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
	async fn mcp_resource_page_shows_the_connection_and_escapes_tool_text(db: PgPool) {
		let s = setup(db).await;
		let cookie = s.cookie().await;
		let files = s.mcp("Files").await;
		*s.agent.tools_line.lock().unwrap() = json!({
			"tools": [{ "name": "read_file", "description": "<script>alert(1)</script>" }],
		})
		.to_string();
		s.deploy(files).await;

		let (status, _, body) = get_html(&s.app, &s.page(files), Some(&cookie)).await;
		assert_eq!(status, StatusCode::OK, "{body}");
		assert_contains(&body, "http://llm.podkit.internal:8088/mcp/files");
		assert_contains(&body, "List tools");
		assert_eq!(body.matches("<h1").count(), 1);

		let (status, _, patch) = post_signals(
			&s.app,
			&format!("{}/tools", s.page(files)),
			Some(&cookie),
			&json!({}),
		)
		.await;
		assert_eq!(status, StatusCode::OK, "{patch}");
		assert_contains(&patch, "read_file");
		assert_contains(&patch, "&lt;script&gt;");
		assert_not_contains(&patch, "<script>alert");
	}

	#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
	async fn mcp_resource_app_tab_links_and_unlinks_without_showing_a_key(db: PgPool) {
		let s = setup(db).await;
		let cookie = s.cookie().await;
		let files = s.mcp("Files").await;

		let (status, _, body) = get_html(&s.app, &s.tab(), Some(&cookie)).await;
		assert_eq!(status, StatusCode::OK, "{body}");
		assert_contains(&body, "Link an MCP server");

		let (status, _, patch) = post_signals(
			&s.app,
			&format!("{}/link", s.tab()),
			Some(&cookie),
			&json!({ "mcpServer": files.to_string() }),
		)
		.await;
		assert_eq!(status, StatusCode::OK, "{patch}");
		assert_contains(&patch, "MCP_FILES_URL");
		let hash = s.agent.configs.lock().unwrap().last().unwrap().mcp_keys[0]
			.key_hash
			.clone();
		assert_not_contains(&patch, &hash);

		let (status, _, patch) = post_signals(
			&s.app,
			&format!("{}/unlink?server={files}", s.tab()),
			Some(&cookie),
			&json!({}),
		)
		.await;
		assert_eq!(status, StatusCode::OK, "{patch}");
		assert_contains(&patch, "No MCP server is linked");
	}

	#[sqlx::test(migrator = "podkit_core::db::MIGRATOR")]
	async fn mcp_resource_pages_refuse_other_teams_and_anonymous_callers(db: PgPool) {
		let s = setup(db).await;
		let files = s.mcp("Files").await;
		user_with_team(&s.app, "mallory").await;
		let mallory = ui_login(&s.app, "mallory").await;

		let (status, _, _) = get_html(&s.app, &s.page(files), Some(&mallory)).await;
		assert_eq!(status, StatusCode::NOT_FOUND);
		let (status, _, _) = get_html(&s.app, &s.tab(), Some(&mallory)).await;
		assert_eq!(status, StatusCode::NOT_FOUND);
		let (status, _, _) = post_signals(
			&s.app,
			&format!("{}/link", s.tab()),
			Some(&mallory),
			&json!({ "mcpServer": files.to_string() }),
		)
		.await;
		// An action's refusal is an event on the page's stream, not a status.
		assert_eq!(status, StatusCode::OK);
		assert_eq!(s.link_count().await, 0);
		assert!(
			s.agent
				.configs
				.lock()
				.unwrap()
				.iter()
				.all(|c| c.mcp_keys.is_empty())
		);

		let (status, _, _) = get_html(&s.app, &s.page(files), None).await;
		assert!(
			status.is_redirection() || status == StatusCode::UNAUTHORIZED,
			"{status}"
		);
	}
}
