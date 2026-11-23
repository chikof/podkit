//! The Monitoring tab: the application's CPU and memory over 1 hour, 24 hours or 7 days, from
//! the sampler's readings (`metrics::service::application` checks the permission and the
//! team). `GET …/monitoring/charts` is the 30-second refresh, which patches the charts alone.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::{Markup, html};
use serde::Deserialize;

use super::{Tab, app_url, forbid_unless_allowed, load, page, replica_list};
use crate::ids::{ApplicationId, ProjectId, TeamId};
use crate::metrics::service::{Target, extras as load_extras};
use crate::metrics::{Range, service as metrics_service};
use crate::replicas::service as replicas_service;
use crate::state::AppState;
use crate::ui::components::{Flavor, MetricsSection, metrics_section, resource_monitoring};
use crate::ui::extract::TeamCtx;
use crate::ui::{error, sse};

const APP: &str = "/teams/{team_id}/projects/{project_id}/apps/{app_id}";

pub fn router() -> Router<AppState> {
	Router::new()
		.route(&format!("{APP}/monitoring"), get(monitoring_page))
		.route(&format!("{APP}/monitoring/charts"), get(charts))
		.route(&format!("{APP}/monitoring/replicas"), get(replicas))
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
	let extras = load_extras(
		&s.db,
		ctx.user.id,
		ctx.team.id,
		Target::Application(project.id, app.id),
		range,
	)
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
	let body = resource_monitoring(Flavor::Application, &points, &extras, range, now());
	let content = html! {
		(replica_section(&s, &ctx, project.id, app.id).await)
		(metrics_section(&section, &body))
	};
	Ok(page(&s, &ctx, &project, &app, Tab::Monitoring, &content).await)
}

/// The replica table, when the application runs several copies. The page and the refresh
/// share it, so the same rows come back every 30 seconds.
async fn replica_section(
	s: &AppState,
	ctx: &TeamCtx,
	project: ProjectId,
	app: ApplicationId,
) -> Markup {
	let refresh = format!("{}/monitoring/replicas", app_url(ctx.team.id, project, app));
	if let Ok(Some(live)) =
		replicas_service::status(s, ctx.user.id, ctx.team.id, project, app).await
	{
		replica_list::section(&live, &refresh)
	} else {
		html! {}
	}
}

/// The refresh of the replica table alone, as a Datastar patch.
async fn replicas(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
) -> Response {
	match replicas_service::status(&s, ctx.user.id, ctx.team.id, project, app).await {
		Ok(Some(live)) => {
			let refresh = format!("{}/monitoring/replicas", app_url(ctx.team.id, project, app));
			sse::patch(&replica_list::section(&live, &refresh)).into_response()
		}
		Ok(None) => axum::http::StatusCode::NO_CONTENT.into_response(),
		Err(e) => error::action(&e),
	}
}

/// The refresh: the charts alone, as a Datastar patch.
async fn charts(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, app)): Path<(TeamId, ProjectId, ApplicationId)>,
	Query(query): Query<RangeQuery>,
) -> Response {
	let range = query.range();
	let team = ctx.team.id;
	let points = metrics_service::application(&s.db, ctx.user.id, team, project, app, range).await;
	let extras = load_extras(
		&s.db,
		ctx.user.id,
		team,
		Target::Application(project, app),
		range,
	)
	.await;
	match (points, extras) {
		(Ok(points), Ok(extras)) => sse::patch(&resource_monitoring(
			Flavor::Application,
			&points,
			&extras,
			range,
			now(),
		))
		.into_response(),
		(Err(e), _) | (_, Err(e)) => error::action(&e),
	}
}
