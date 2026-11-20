//! Metric alerts: "metric X over Y for Z minutes" on a server or on one resource, judged from
//! the rows the metrics sampler already stores. [`evaluate`] runs after every sampler sweep;
//! it fires an event once the value has stayed over the threshold for the whole duration, and
//! a recovery event when it is back under.

use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

use super::event::{Event, EventKind, Severity};
use super::hooks;
use crate::ids::{MetricAlertId, ServerId, TeamId};
use crate::state::AppState;

/// A reading older than this says nothing about now (the sampler or the server is down).
const STALE: Duration = Duration::minutes(5);

/// What an alert watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
	Cpu,
	/// Percent of the server's memory.
	Mem,
	Disk,
	Gpu,
	/// A resource's memory in MiB.
	MemMb,
}

impl Metric {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Cpu => "cpu",
			Self::Mem => "mem",
			Self::Disk => "disk",
			Self::Gpu => "gpu",
			Self::MemMb => "mem_mb",
		}
	}

	pub fn parse(s: &str) -> Option<Self> {
		[Self::Cpu, Self::Mem, Self::Disk, Self::Gpu, Self::MemMb]
			.into_iter()
			.find(|m| m.as_str() == s)
	}

	pub fn label(self) -> &'static str {
		match self {
			Self::Cpu => "CPU",
			Self::Mem | Self::MemMb => "Memory",
			Self::Disk => "Disk",
			Self::Gpu => "GPU",
		}
	}

	pub fn unit(self) -> &'static str {
		match self {
			Self::MemMb => " MiB",
			_ => "%",
		}
	}
}

/// A row of `metric_alerts`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MetricAlert {
	pub id: MetricAlertId,
	pub team_id: TeamId,
	pub name: String,
	/// `server` or `resource`.
	pub scope: String,
	pub server_id: Option<ServerId>,
	pub resource_kind: Option<String>,
	pub resource_id: Option<i64>,
	pub metric: String,
	pub threshold: f64,
	pub duration_minutes: i32,
	pub severity: i16,
	pub enabled: bool,
	pub state: String,
	pub breach_since: Option<OffsetDateTime>,
}

pub const ALERT_COLUMNS: &str = "id, team_id, name, scope, server_id, resource_kind, resource_id, \
	metric, threshold, duration_minutes, severity, enabled, state, breach_since";

/// What a reading did to an alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
	Nothing,
	Fire,
	Resolve,
}

/// The state machine: `state` is `ok` or `firing`, `since` when the value first went over.
/// Returns the new state, the new `since` and what to announce.
pub fn step(
	state: &str,
	since: Option<OffsetDateTime>,
	over: bool,
	now: OffsetDateTime,
	duration: Duration,
) -> (&'static str, Option<OffsetDateTime>, Step) {
	match (state == "firing", over) {
		(false, true) => {
			let since = since.unwrap_or(now);
			if now - since >= duration {
				("firing", Some(since), Step::Fire)
			} else {
				("ok", Some(since), Step::Nothing)
			}
		}
		(false, false) => ("ok", None, Step::Nothing),
		(true, true) => ("firing", since, Step::Nothing),
		(true, false) => ("ok", None, Step::Resolve),
	}
}

async fn server_value(
	db: &PgPool,
	server: ServerId,
	metric: Metric,
	now: OffsetDateTime,
) -> Option<f64> {
	let row = sqlx::query_as::<_, (OffsetDateTime, Option<f64>, i64, i64, i64, i64, Option<f64>)>(
		"SELECT at, cpu, mem_used, mem_total, disk_used, disk_total, gpu_util
		FROM server_metrics WHERE server_id = $1 AND NOT rollup ORDER BY at DESC LIMIT 1",
	)
	.bind(server)
	.fetch_optional(db)
	.await
	.ok()
	.flatten()?;
	let (at, cpu, mem_used, mem_total, disk_used, disk_total, gpu) = row;
	if now - at > STALE {
		return None;
	}
	let percent = |used: i64, total: i64| (total > 0).then(|| used as f64 * 100.0 / total as f64);
	match metric {
		Metric::Cpu => cpu,
		Metric::Mem => percent(mem_used, mem_total),
		Metric::Disk => percent(disk_used, disk_total),
		Metric::Gpu => gpu,
		Metric::MemMb => None,
	}
}

async fn resource_value(
	db: &PgPool,
	kind: &str,
	id: i64,
	metric: Metric,
	now: OffsetDateTime,
) -> Option<f64> {
	let (at, cpu, mem) = sqlx::query_as::<_, (OffsetDateTime, Option<f64>, i64)>(
		"SELECT at, cpu, mem FROM resource_metrics
		WHERE kind = $1 AND resource_id = $2 AND NOT rollup ORDER BY at DESC LIMIT 1",
	)
	.bind(kind)
	.bind(id)
	.fetch_optional(db)
	.await
	.ok()
	.flatten()?;
	if now - at > STALE {
		return None;
	}
	match metric {
		Metric::Cpu => cpu,
		Metric::MemMb => Some(mem as f64 / (1024.0 * 1024.0)),
		_ => None,
	}
}

/// Judges every enabled alert against its newest reading. Returns how many events were raised.
pub async fn evaluate(state: &AppState, now: OffsetDateTime) -> Result<usize, sqlx::Error> {
	let alerts = sqlx::query_as::<_, MetricAlert>(sqlx::AssertSqlSafe(format!(
		"SELECT {ALERT_COLUMNS} FROM metric_alerts WHERE enabled"
	)))
	.fetch_all(&state.db)
	.await?;
	let mut raised = 0;
	for alert in alerts {
		let Some(metric) = Metric::parse(&alert.metric) else {
			continue;
		};
		let value = if alert.scope == "server" {
			match alert.server_id {
				Some(server) => server_value(&state.db, server, metric, now).await,
				None => None,
			}
		} else {
			match (&alert.resource_kind, alert.resource_id) {
				(Some(kind), Some(id)) => resource_value(&state.db, kind, id, metric, now).await,
				_ => None,
			}
		};
		let Some(value) = value else { continue };
		let (next, since, action) = step(
			&alert.state,
			alert.breach_since,
			value > alert.threshold,
			now,
			Duration::minutes(i64::from(alert.duration_minutes)),
		);
		if next != alert.state || since != alert.breach_since {
			sqlx::query(
				"UPDATE metric_alerts SET state = $2, breach_since = $3,
					fired_at = CASE WHEN $4 THEN $5 ELSE fired_at END WHERE id = $1",
			)
			.bind(alert.id)
			.bind(next)
			.bind(since)
			.bind(action == Step::Fire)
			.bind(now)
			.execute(&state.db)
			.await?;
		}
		if action != Step::Nothing {
			announce(state, &alert, metric, value, action == Step::Fire).await;
			raised += 1;
		}
	}
	Ok(raised)
}

fn figure(value: f64, metric: Metric) -> String {
	format!("{value:.0}{}", metric.unit())
}

async fn announce(state: &AppState, alert: &MetricAlert, metric: Metric, value: f64, firing: bool) {
	// What the alert watches, for the title and for the rules' project and environment.
	let (what, link, project, environment) = if alert.scope == "server" {
		let Some(server) = alert.server_id else {
			return;
		};
		let Some((_, name)) = hooks::server(&state.db, server.0).await else {
			return;
		};
		(
			format!("on {name}"),
			format!("/teams/{}/servers/{server}", alert.team_id),
			None,
			None,
		)
	} else {
		let (Some(kind), Some(id)) = (&alert.resource_kind, alert.resource_id) else {
			return;
		};
		let Some(target) = hooks::by_kind(&state.db, kind, id).await else {
			return;
		};
		(
			format!("of {}", target.name),
			target.link,
			Some(target.project),
			Some(target.environment),
		)
	};
	let threshold = figure(alert.threshold, metric);
	let (kind, title, severity) = if firing {
		let kind = if metric == Metric::Disk && alert.scope == "server" {
			EventKind::DiskHigh
		} else {
			EventKind::MetricAlert
		};
		(
			kind,
			format!("{} {what} is at {}", metric.label(), figure(value, metric)),
			Severity::from_rank(alert.severity),
		)
	} else {
		(
			EventKind::MetricRecovered,
			format!(
				"{} {what} is back to {}",
				metric.label(),
				figure(value, metric)
			),
			Severity::Info,
		)
	};
	let body = if firing {
		format!(
			"{}: over {threshold} for {} min.",
			alert.name, alert.duration_minutes
		)
	} else {
		format!("{}: back under {threshold}.", alert.name)
	};
	let mut event = Event::new(alert.team_id, kind, format!("alert:{}", alert.id), title)
		.body(body)
		.severity(severity)
		.link(link);
	event.project = project;
	event.environment = environment;
	super::emit(state, event).await;
}

#[cfg(test)]
mod tests {
	use time::macros::datetime;

	use super::*;

	#[test]
	fn fires_only_after_the_whole_duration() {
		let t0 = datetime!(2026-01-01 12:00 UTC);
		let five = Duration::minutes(5);
		let (state, since, step1) = step("ok", None, true, t0, five);
		assert_eq!((state, since, step1), ("ok", Some(t0), Step::Nothing));
		let (state, since, step2) = step(state, since, true, t0 + Duration::minutes(4), five);
		assert_eq!((state, step2), ("ok", Step::Nothing));
		let (state, since, step3) = step(state, since, true, t0 + Duration::minutes(5), five);
		assert_eq!((state, step3), ("firing", Step::Fire));
		// Still over: nothing more to say.
		let (state, since, step4) = step(state, since, true, t0 + Duration::minutes(9), five);
		assert_eq!((state, step4), ("firing", Step::Nothing));
		let (state, since, step5) = step(state, since, false, t0 + Duration::minutes(10), five);
		assert_eq!((state, since, step5), ("ok", None, Step::Resolve));
	}

	#[test]
	fn a_dip_resets_the_clock() {
		let t0 = datetime!(2026-01-01 12:00 UTC);
		let five = Duration::minutes(5);
		let (state, since, _) = step("ok", None, true, t0, five);
		let (state, since, action) = step(state, since, false, t0 + Duration::minutes(3), five);
		assert_eq!((state, since, action), ("ok", None, Step::Nothing));
		let (_, since, action) = step("ok", since, true, t0 + Duration::minutes(6), five);
		assert_eq!(
			(since, action),
			(Some(t0 + Duration::minutes(6)), Step::Nothing)
		);
	}
}
