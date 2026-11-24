//! The Monitoring tab: the stack's CPU and memory (all its containers together) over 1 hour,
//! 24 hours or 7 days. `GET …/monitoring/charts` is the 30-second refresh, which patches the
//! charts alone. `metrics::service::stack` checks the permission and the team.

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use maud::Markup;
use serde::Deserialize;

use super::frame::{self, Tab};
use crate::components::{Flavor, MetricsSection, metrics_section, resource_monitoring};
use crate::extract::TeamCtx;
use crate::{error, sse};
use podkit_core::ids::{ProjectId, StackId, TeamId};
use podkit_core::metrics::service::{Target, extras as load_extras};
use podkit_core::metrics::{Range, service as metrics_service};
use podkit_core::state::AppState;

#[derive(Deserialize)]
pub(super) struct RangeQuery {
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

pub(super) async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
	Query(query): Query<RangeQuery>,
) -> Result<Markup, Response> {
	let range = query.range();
	let l = frame::load(&s, &ctx, project, id).await?;
	let points = metrics_service::stack(&s.db, ctx.user.id, ctx.team.id, project, id, range)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let extras = load_extras(
		&s.db,
		ctx.user.id,
		ctx.team.id,
		Target::Stack(project, id),
		range,
	)
	.await
	.map_err(|e| error::page(&ctx, &e))?;
	let page_url = format!("{}/monitoring", l.base(ctx.team.id));
	let refresh_url = format!("{page_url}/charts");
	let section = MetricsSection {
		id: "metrics",
		page_url: &page_url,
		refresh_url: &refresh_url,
		range,
	};
	let body = resource_monitoring(Flavor::Stack, &points, &extras, range, now());
	let content = metrics_section(&section, &body);
	Ok(frame::page(&ctx, &l, Tab::Monitoring, &content))
}

/// The refresh: the charts alone, as a Datastar patch.
pub(super) async fn charts(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, project, id)): Path<(TeamId, ProjectId, StackId)>,
	Query(query): Query<RangeQuery>,
) -> Response {
	let range = query.range();
	let team = ctx.team.id;
	let points = metrics_service::stack(&s.db, ctx.user.id, team, project, id, range).await;
	let extras = load_extras(&s.db, ctx.user.id, team, Target::Stack(project, id), range).await;
	match (points, extras) {
		(Ok(points), Ok(extras)) => sse::patch(&resource_monitoring(
			Flavor::Stack,
			&points,
			&extras,
			range,
			now(),
		))
		.into_response(),
		(Err(e), _) | (_, Err(e)) => error::action(&e),
	}
}
