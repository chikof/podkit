//! The Monitoring tab: the database's CPU and memory over 1 hour, 24 hours or 7 days, from
//! the sampler's readings (`metrics::service::database` checks the permission and the team).
//! `GET …/monitoring/charts` is the 30-second refresh, which patches the charts alone.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::Markup;
use serde::Deserialize;

use super::{Tab, database_url, forbid_unless_allowed, load, page};
use crate::ids::{DatabaseId, ProjectId, TeamId};
use crate::metrics::{Range, service as metrics_service};
use crate::state::AppState;
use crate::ui::components::{MetricsSection, metrics_section, resource_charts};
use crate::ui::extract::TeamCtx;
use crate::ui::{error, sse};

const DB: &str = "/teams/{team_id}/projects/{project_id}/databases/{database_id}";

pub fn router() -> Router<AppState> {
	Router::new()
		.route(&format!("{DB}/monitoring"), get(monitoring_page))
		.route(&format!("{DB}/monitoring/charts"), get(charts))
}

#[derive(Deserialize)]
struct RangeQuery {
	range: Option<String>,
}

impl RangeQuery {
	/// An unknown or missing range is the default one, not an error: the link may be old.
	fn range(&self) -> Range {
		self.range
			.as_deref()
			.and_then(Range::parse)
			.unwrap_or(Range::Hour)
	}
}

fn now() -> i64 {
	time::OffsetDateTime::now_utc().unix_timestamp()
}

async fn monitoring_page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
	Query(query): Query<RangeQuery>,
) -> Result<Markup, Response> {
	forbid_unless_allowed(&ctx, Tab::Monitoring)?;
	let range = query.range();
	let (project, d) = load(&s, &ctx, project, database).await?;
	let points = metrics_service::database(
		&s.db,
		ctx.user.id,
		ctx.team.id,
		project.id,
		d.database.id,
		range,
	)
	.await
	.map_err(|e| error::page(&ctx, &e))?;
	let page_url = format!(
		"{}/monitoring",
		database_url(ctx.team.id, project.id, d.database.id)
	);
	let refresh_url = format!("{page_url}/charts");
	let section = MetricsSection {
		id: "metrics",
		page_url: &page_url,
		refresh_url: &refresh_url,
		range,
	};
	let content = metrics_section(&section, &resource_charts(&points, range, now()));
	Ok(page(&s, &ctx, &project, &d, Tab::Monitoring, &content).await)
}

/// The refresh: the charts alone, as a Datastar patch.
async fn charts(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, database)): Path<(TeamId, ProjectId, DatabaseId)>,
	Query(query): Query<RangeQuery>,
) -> Response {
	let range = query.range();
	match metrics_service::database(&s.db, ctx.user.id, ctx.team.id, project, database, range).await
	{
		Ok(points) => sse::patch(&resource_charts(&points, range, now())).into_response(),
		Err(e) => error::action(&e),
	}
}
