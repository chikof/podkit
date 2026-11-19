//! A server's Monitoring tab: CPU, memory, disk, network and (with a GPU) GPU and VRAM
//! charts from the sampler's readings, over 1 hour, 24 hours or 7 days. The charts are
//! inline SVG from `ui::charts`; `GET …/monitoring/charts` is the 30-second refresh.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::{Markup, html};
use serde::Deserialize;

use super::detail::header;
use super::{crumbs, monitoring_url};
use crate::ids::{ServerId, TeamId};
use crate::metrics::{Range, ServerPoint, service as metrics_service};
use crate::servers::{Server, service};
use crate::state::AppState;
use crate::ui::charts::{Chart, Series, Unit, line_chart};
use crate::ui::components::{MetricsSection, metrics_body, metrics_section, range_phrase};
use crate::ui::extract::TeamCtx;
use crate::ui::layout::app_page;
use crate::ui::{error, sse};

pub fn router() -> Router<AppState> {
	Router::new()
		.route("/teams/{team_id}/servers/{server_id}/monitoring", get(page))
		.route(
			"/teams/{team_id}/servers/{server_id}/monitoring/charts",
			get(charts),
		)
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

/// Bytes per second between consecutive readings of a counter since boot. A reading
/// after a reboot (the counter went down) or after no time passed has no rate.
fn rates(points: &[ServerPoint], counter: impl Fn(&ServerPoint) -> i64) -> Vec<Option<f64>> {
	let mut out = Vec::with_capacity(points.len());
	for (i, p) in points.iter().enumerate() {
		out.push(i.checked_sub(1).and_then(|j| {
			let prev = &points[j];
			let (dt, db) = (p.at - prev.at, counter(p) - counter(prev));
			(dt > 0 && db >= 0).then(|| db as f64 / dt as f64)
		}));
	}
	out
}

/// The capacity of a gauge from its newest reading, so the axis tops out at "all of it".
fn capacity(points: &[ServerPoint], total: impl Fn(&ServerPoint) -> i64) -> Option<f64> {
	points
		.iter()
		.rev()
		.map(total)
		.find(|t| *t > 0)
		.map(|t| t as f64)
}

/// VRAM of the NVIDIA GPUs, which is what the sampler's `vram_used` adds up.
fn vram_total(server: &Server) -> Option<f64> {
	let hw = server.hardware.as_ref().map(|h| &h.0)?;
	let mb: u64 = hw
		.gpus
		.iter()
		.filter(|g| g.vendor == "nvidia")
		.map(|g| g.vram_mb)
		.sum();
	(mb > 0).then_some(mb as f64 * 1024.0 * 1024.0)
}

/// The charts of one server, in a `metrics_body`.
pub(super) fn server_charts(
	server: &Server,
	points: &[ServerPoint],
	range: Range,
	now: i64,
) -> Markup {
	let from = now - range.duration().whole_seconds();
	let xs: Vec<i64> = points.iter().map(|p| p.at).collect();
	let phrase = range_phrase(range);
	let gauge = |id, title, unit, max, color, label, values: Vec<Option<f64>>| {
		let chart = Chart {
			id,
			title,
			range: phrase,
			unit,
			max,
			from,
			to: now,
			xs: &xs,
			series: vec![Series {
				label,
				color,
				values,
			}],
		};
		line_chart(&chart)
	};
	let cpu = gauge(
		"cpu",
		"CPU",
		Unit::Percent,
		Some(100.0),
		2,
		"CPU",
		points.iter().map(|p| p.cpu).collect(),
	);
	let memory = gauge(
		"memory",
		"Memory",
		Unit::Bytes,
		capacity(points, |p| p.mem_total),
		4,
		"Used",
		points.iter().map(|p| Some(p.mem_used as f64)).collect(),
	);
	let disk = gauge(
		"disk",
		"Disk",
		Unit::Bytes,
		capacity(points, |p| p.disk_total),
		3,
		"Used",
		points.iter().map(|p| Some(p.disk_used as f64)).collect(),
	);
	let network = line_chart(&Chart {
		id: "network",
		title: "Network",
		range: phrase,
		unit: Unit::BytesPerSec,
		max: None,
		from,
		to: now,
		xs: &xs,
		series: vec![
			Series {
				label: "Received",
				color: 1,
				values: rates(points, |p| p.net_rx),
			},
			Series {
				label: "Sent",
				color: 5,
				values: rates(points, |p| p.net_tx),
			},
		],
	});
	let has_gpu = points
		.iter()
		.any(|p| p.gpu_util.is_some() || p.vram_used.is_some());
	let gpu = has_gpu.then(|| {
		html! {
			(gauge("gpu", "GPU", Unit::Percent, Some(100.0), 2, "Utilisation", points.iter().map(|p| p.gpu_util).collect()))
			(gauge("vram", "VRAM", Unit::Bytes, vram_total(server), 1, "Used", points.iter().map(|p| p.vram_used.map(|v| v as f64)).collect()))
		}
	});
	metrics_body(
		&html! {
			(cpu) (memory) (disk) (network)
			@if let Some(gpu) = gpu { (gpu) }
		},
		now,
	)
}

async fn page(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
	Query(query): Query<RangeQuery>,
) -> Result<Markup, Response> {
	let range = query.range();
	let found = service::get(&s.db, ctx.user.id, ctx.team.id, id)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let points = metrics_service::server(&s.db, ctx.user.id, ctx.team.id, id, range)
		.await
		.map_err(|e| error::page(&ctx, &e))?;
	let server = &found.server;
	let page_url = monitoring_url(ctx.team.id, id);
	let refresh_url = format!("{page_url}/charts");
	let section = MetricsSection {
		id: "metrics",
		page_url: &page_url,
		refresh_url: &refresh_url,
		range,
	};

	let mut crumbs = crumbs(&ctx);
	crumbs.push((
		server.name.as_str(),
		Some(super::detail_url(ctx.team.id, id)),
	));
	crumbs.push(("Monitoring", None));
	let body = html! {
		div class="ui-page-body" {
			(header(ctx.team.id, server, true, &html! {}))
			(metrics_section(&section, &server_charts(server, &points, range, now())))
		}
	};
	Ok(app_page(
		&ctx,
		&format!("{} · Monitoring", server.name),
		&crumbs,
		&body,
	))
}

/// The refresh: the charts alone, as a Datastar patch.
async fn charts(
	State(s): State<AppState>,
	ctx: TeamCtx,
	Path((_, id)): Path<(TeamId, ServerId)>,
	Query(query): Query<RangeQuery>,
) -> Response {
	let range = query.range();
	let result = async {
		let found = service::get(&s.db, ctx.user.id, ctx.team.id, id).await?;
		let points = metrics_service::server(&s.db, ctx.user.id, ctx.team.id, id, range).await?;
		Ok::<_, crate::error::ServerError>((found.server, points))
	}
	.await;
	match result {
		Ok((server, points)) => {
			sse::patch(&server_charts(&server, &points, range, now())).into_response()
		}
		Err(e) => error::action(&e),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ui::components::range_value;

	fn point(at: i64, rx: i64) -> ServerPoint {
		ServerPoint {
			at,
			cpu: Some(10.0),
			mem_used: 1,
			mem_total: 2,
			disk_used: 1,
			disk_total: 2,
			net_rx: rx,
			net_tx: 0,
			gpu_util: None,
			vram_used: None,
		}
	}

	#[test]
	fn network_rates_come_from_counter_differences() {
		let pts = [point(0, 1000), point(10, 6000), point(20, 6000)];
		let r = rates(&pts, |p| p.net_rx);
		assert_eq!(r, vec![None, Some(500.0), Some(0.0)]);
	}

	#[test]
	fn a_reboot_or_a_repeated_timestamp_has_no_rate() {
		let pts = [
			point(0, 9000),
			point(10, 100),
			point(10, 200),
			point(20, 400),
		];
		let r = rates(&pts, |p| p.net_rx);
		assert_eq!(r, vec![None, None, None, Some(20.0)]);
	}

	#[test]
	fn capacity_is_the_newest_nonzero_total() {
		let mut pts = [point(0, 0), point(1, 0)];
		pts[0].mem_total = 100;
		pts[1].mem_total = 0;
		assert_eq!(capacity(&pts, |p| p.mem_total), Some(100.0));
		assert_eq!(capacity(&[], |p| p.mem_total), None);
	}

	#[test]
	fn the_range_parameter_falls_back_to_the_last_hour() {
		for (given, want) in [
			(Some("24h"), Range::Day),
			(Some("7d"), Range::Week),
			(Some("1h"), Range::Hour),
			(Some("90d"), Range::Hour),
			(None, Range::Hour),
		] {
			let q = RangeQuery {
				range: given.map(str::to_owned),
			};
			assert_eq!(q.range(), want);
			assert_eq!(Range::parse(range_value(q.range())), Some(q.range()));
		}
	}
}
