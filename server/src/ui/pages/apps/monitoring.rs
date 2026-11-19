//! The Monitoring tab: the application's CPU and memory over 1 hour, 24 hours or 7 days, from
//! the sampler's readings (`metrics::service::application` checks the permission and the
//! team). `GET …/monitoring/charts` is the 30-second refresh, which patches the charts alone.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::Markup;
use serde::Deserialize;

use super::{Tab, app_url, forbid_unless_allowed, load, page};
use crate::ids::{ApplicationId, ProjectId, TeamId};
use crate::metrics::{Range, service as metrics_service};
use crate::state::AppState;
use crate::ui::components::{MetricsSection, metrics_section, resource_charts};
use crate::ui::extract::TeamCtx;
use crate::ui::{error, sse};

const APP: &str = "/teams/{team_id}/projects/{project_id}/apps/{app_id}";

pub fn router() -> Router<AppState> {
	Router::new()
		.route(&format!("{APP}/monitoring"), get(monitoring_page))
		.route(&format!("{APP}/monitoring/charts"), get(charts))
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
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	Query(query): Query<RangeQuery>,
) -> Result<Markup, Response> {
	forbid_unless_allowed(&ctx, Tab::Monitoring)?;
	let range = query.range();
	let (project, app) = load(&s, &ctx, project, app).await?;
	let points =
		metrics_service::application(&s.db, ctx.user.id, ctx.team.id, project.id, app.id, range)
			.await
			.map_err(|e| error::page(&ctx, &e))?;
	let page_url = format!("{}/monitoring", app_url(ctx.team.id, project.id, app.id));
	let refresh_url = format!("{page_url}/charts");
	let section = MetricsSection {
		id: "metrics",
		page_url: &page_url,
		refresh_url: &refresh_url,
		range,
	};
	let content = metrics_section(&section, &resource_charts(&points, range, now()));
	Ok(page(&s, &ctx, &project, &app, Tab::Monitoring, &content).await)
}

/// The refresh: the charts alone, as a Datastar patch.
async fn charts(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	Query(query): Query<RangeQuery>,
) -> Response {
	let range = query.range();
	match metrics_service::application(&s.db, ctx.user.id, ctx.team.id, project, app, range).await {
		Ok(points) => sse::patch(&resource_charts(&points, range, now())).into_response(),
		Err(e) => error::action(&e),
	}
}
