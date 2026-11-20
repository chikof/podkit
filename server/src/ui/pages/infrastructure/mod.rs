//! Infrastructure: one place that shows what runs where, and the guided setup that gets a
//! team there. The sidebar lists this section's pages (Servers, Networks, Keys, Registries,
//! Git providers, Security, S3 destinations); this folder owns the pages that are not
//! anyone else's:
//!
//! - `overview`: per server its state, proxy, networks, residents, scanner and load, then
//!   the "what runs where" table. A **Monitor** surface.
//! - `networks`: every network of every server, with who is on it.
//! - `setup`: the six-step guide, a **Configure** surface, and the checklist the overview
//!   shows until it is done or dismissed.
//!
//! The page reads and acts only through `infrastructure::service`, which checks the
//! permission and the team, so another team's id is a 404 like any other.

use axum::Router;
use axum::routing::{get, post};

use crate::ids::TeamId;
use crate::state::AppState;
use crate::ui::extract::TeamCtx;
use crate::ui::layout::Crumb;

mod networks;
mod overview;
mod setup;
#[cfg(test)]
mod tests;

const BASE: &str = "/teams/{team_id}/infrastructure";

pub fn router() -> Router<AppState> {
	let at = |path: &str| format!("{BASE}{path}");
	Router::new()
		.route(BASE, get(overview::page))
		.route(&at("/networks"), get(networks::page))
		.route(&at("/setup"), get(setup::page))
		.route(&at("/setup/dismiss"), post(setup::dismiss))
		.route(&at("/setup/reopen"), post(setup::reopen))
		.route(&at("/setup/skip/{item}"), post(setup::skip))
		.route(&at("/setup/unskip/{item}"), post(setup::unskip))
}

pub(in crate::ui) fn overview_url(team: TeamId) -> String {
	format!("/teams/{team}/infrastructure")
}

pub(in crate::ui) fn setup_url(team: TeamId) -> String {
	format!("/teams/{team}/infrastructure/setup")
}

fn crumbs<'a>(ctx: &TeamCtx) -> Vec<Crumb<'a>> {
	vec![("Infrastructure", Some(overview_url(ctx.team.id)))]
}
