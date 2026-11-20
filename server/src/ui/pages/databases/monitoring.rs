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
use crate::metrics::service::{Target, extras as load_extras};
use crate::metrics::{Range, service as metrics_service};
use crate::state::AppState;
use crate::ui::components::{Flavor, MetricsSection, Ops, metrics_section, resource_monitoring};
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

/// What the engine's activity counter is called on the chart.
fn flavor_of(engine: proto::Engine) -> Flavor {
	Flavor::Database {
		ops: if engine == proto::Engine::Postgres {
			Ops::Transactions
		} else {
			Ops::Commands
		},
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
	let extras = load_extras(
		&s.db,
		ctx.user.id,
		ctx.team.id,
		Target::Database(project.id, d.database.id),
		range,
	)
	.await
	.map_err(|e| error::page(&ctx, &e))?;
	let flavor = flavor_of(d.database.engine());
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
	let body = resource_monitoring(flavor, &points, &extras, range, now());
	let content = metrics_section(&section, &body);
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
	let team = ctx.team.id;
	let points =
		metrics_service::database(&s.db, ctx.user.id, team, project, database, range).await;
	let extras = load_extras(
		&s.db,
		ctx.user.id,
		team,
		Target::Database(project, database),
		range,
	)
	.await;
	let engine = crate::databases::service::find(&s.db, ctx.user.id, team, project, database)
		.await
		.map(|d| d.engine());
	match (points, extras) {
		(Ok(points), Ok(extras)) => {
			let flavor = flavor_of(engine.unwrap_or(proto::Engine::Postgres));
			sse::patch(&resource_monitoring(flavor, &points, &extras, range, now())).into_response()
		}
		(Err(e), _) | (_, Err(e)) => error::action(&e),
	}
}
