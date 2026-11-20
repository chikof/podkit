//! Compose stacks in the web UI: create one from a pasted `docker-compose.yml` (with a
//! validation panel that names every refused key), and a stack's own page with its
//! services, compose file, variables, domains, deployments, logs and removal.
//!
//! A stack's variables are write-only here as everywhere: the page lists their names and
//! never a value. Every action calls `stacks::service`, which authorizes and scopes.

use axum::Router;
use axum::routing::{get, post};

use crate::ids::{ProjectId, TeamId};
use crate::state::AppState;

mod compose;
mod create;
mod danger;
mod domains;
mod env;
mod frame;
mod issues;
mod monitoring;
mod networks;
mod runs;
mod services;
#[cfg(test)]
mod tests;

const PROJECT: &str = "/teams/{team_id}/projects/{project_id}/stacks";
const STACK: &str = "/teams/{team_id}/projects/{project_id}/stacks/{stack_id}";

pub fn router() -> Router<AppState> {
	let project = |path: &str| format!("{PROJECT}{path}");
	let at = |path: &str| format!("{STACK}{path}");
	Router::new()
		.route(&project("/new"), get(create::new_page))
		.route(&project("/validate"), post(create::validate))
		.route(&project(""), post(create::create))
		.route(&at(""), get(services::page))
		.route(&at("/status"), get(services::status))
		.route(&at("/deploy"), post(frame::deploy))
		.route(&at("/stop"), post(frame::stop))
		.route(&at("/services/{service}/restart"), post(services::restart))
		.route(&at("/compose"), get(compose::page).post(compose::save))
		.route(&at("/env"), get(env::page).post(env::set))
		.route(&at("/env/unset"), post(env::unset))
		.route(&at("/domains"), get(domains::page))
		.route(&at("/domains/{service}"), post(domains::set))
		.route(&at("/domains/{service}/clear"), post(domains::clear))
		.route(&at("/deployments"), get(runs::deployments))
		.route(&at("/deployments/log"), get(runs::deployment_log))
		.route(&at("/logs"), get(runs::logs))
		.route(&at("/logs/stream"), get(runs::logs_stream))
		.route(&at("/monitoring"), get(monitoring::page))
		.route(&at("/monitoring/charts"), get(monitoring::charts))
		.route(&at("/networks"), get(networks::page))
		.route(&at("/advanced"), get(danger::page))
		.route(&at("/remove"), post(danger::remove))
}

pub(super) fn project_url(team: TeamId, project: ProjectId) -> String {
	format!("/teams/{team}/projects/{project}")
}

pub(super) fn stack_url(team: TeamId, project: ProjectId, stack: i64) -> String {
	format!("/teams/{team}/projects/{project}/stacks/{stack}")
}
