//! MCP server pages: a server's own page (how it is reached, its tools listed live, the
//! applications linked to it, usage, log) and the form that makes one.
//!
//! An MCP server is an application, so deploys, environment, volumes and the rest stay on
//! the application's pages; this area is what is specific to MCP. Handlers call the
//! `mcp_servers` service, which authorizes, scopes and validates. A tool's name and
//! description come from the server itself and only ever pass through maud's escaping.

use axum::Router;

use crate::ids::{ApplicationId, ProjectId, TeamId};
use crate::state::AppState;

mod detail;
mod new;

pub fn router() -> Router<AppState> {
	Router::new().merge(new::router()).merge(detail::router())
}

/// A server's page.
pub fn mcp_url(team: TeamId, project: ProjectId, app: ApplicationId) -> String {
	format!("/teams/{team}/projects/{project}/mcp/{app}")
}

/// The form that makes one.
pub fn new_url(team: TeamId, project: ProjectId) -> String {
	format!("/teams/{team}/projects/{project}/mcp/new")
}
