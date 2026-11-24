//! The team's busiest resources: the few that use the most CPU or memory, answer the most 5xx
//! responses, or restart the most. One query over what the sampler already stores, scoped to
//! the team's projects, so another team's resources never appear.

use sqlx::PgPool;

use super::Kind;

/// How many resources each list shows.
pub const TOP: usize = 5;

/// One resource with its recent figures: CPU, memory and HTTP averaged over the last hour,
/// restarts counted over the last 24 hours.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Busy {
	pub kind: String,
	pub id: i64,
	pub project_id: i64,
	pub name: String,
	/// Share of the host's CPU, 0 to 100.
	pub cpu: Option<f64>,
	pub mem: Option<f64>,
	pub req_rate: Option<f64>,
	pub r5xx_rate: Option<f64>,
	/// Crashes (exit status other than 0) and restarts.
	pub restarts: i64,
}

/// The four lists, each at most [`TOP`] long and holding only resources with something to show.
#[allow(
	clippy::struct_field_names,
	reason = "the lists are named by what they rank"
)]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Busiest {
	pub by_cpu: Vec<Busy>,
	pub by_memory: Vec<Busy>,
	pub by_errors: Vec<Busy>,
	pub by_restarts: Vec<Busy>,
}

impl Busiest {
	/// Whether there is nothing to list.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.by_cpu.is_empty()
			&& self.by_memory.is_empty()
			&& self.by_errors.is_empty()
			&& self.by_restarts.is_empty()
	}
}

/// Ranks `rows` into the four lists. Pure, so the ordering is tested without a database.
#[must_use]
pub fn rank(rows: &[Busy]) -> Busiest {
	fn top(rows: &[Busy], keep: impl Fn(&Busy) -> bool, key: impl Fn(&Busy) -> f64) -> Vec<Busy> {
		let mut picked: Vec<Busy> = rows.iter().filter(|r| keep(r)).cloned().collect();
		picked.sort_by(|a, b| key(b).total_cmp(&key(a)).then_with(|| a.name.cmp(&b.name)));
		picked.truncate(TOP);
		picked
	}
	Busiest {
		by_cpu: top(
			rows,
			|r| r.cpu.is_some_and(|c| c > 0.0),
			|r| r.cpu.unwrap_or(0.0),
		),
		by_memory: top(
			rows,
			|r| r.mem.is_some_and(|m| m > 0.0),
			|r| r.mem.unwrap_or(0.0),
		),
		by_errors: top(
			rows,
			|r| r.r5xx_rate.is_some_and(|e| e > 0.0),
			|r| r.r5xx_rate.unwrap_or(0.0),
		),
		#[allow(clippy::cast_precision_loss)]
		by_restarts: top(rows, |r| r.restarts > 0, |r| r.restarts as f64),
	}
}

/// The team's resources of `kinds` with their recent figures.
///
/// # Errors
/// The database rejects the read.
pub async fn load(db: &PgPool, team: i64, kinds: &[Kind]) -> Result<Vec<Busy>, sqlx::Error> {
	let kinds: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
	sqlx::query_as::<_, Busy>(
		"WITH res AS (
			SELECT 'application'::varchar AS kind, a.id, a.project_id, a.name
				FROM applications a JOIN projects p ON p.id = a.project_id WHERE p.team_id = $1
			UNION ALL
			SELECT 'database', d.id, d.project_id, d.name
				FROM databases d JOIN projects p ON p.id = d.project_id WHERE p.team_id = $1
			UNION ALL
			SELECT 'model', m.id, m.project_id, m.name
				FROM models m JOIN projects p ON p.id = m.project_id WHERE p.team_id = $1
			UNION ALL
			SELECT 'stack', s.id, s.project_id, s.name
				FROM stacks s JOIN projects p ON p.id = s.project_id WHERE p.team_id = $1
		)
		SELECT res.kind, res.id, res.project_id, res.name,
			m.cpu, m.mem, x.req_rate, x.r5xx_rate, COALESCE(e.n, 0) AS restarts
		FROM res
		LEFT JOIN LATERAL (
			SELECT AVG(cpu) AS cpu, AVG(mem)::DOUBLE PRECISION AS mem FROM resource_metrics rm
			WHERE rm.kind = res.kind AND rm.resource_id = res.id AND NOT rm.rollup
				AND rm.at > NOW() - INTERVAL '1 hour'
		) m ON TRUE
		LEFT JOIN LATERAL (
			SELECT AVG(req_rate) AS req_rate, AVG(r5xx_rate) AS r5xx_rate
			FROM resource_extra_metrics xm
			WHERE xm.kind = res.kind AND xm.resource_id = res.id AND NOT xm.rollup
				AND xm.at > NOW() - INTERVAL '1 hour'
		) x ON TRUE
		LEFT JOIN LATERAL (
			SELECT COUNT(*) AS n FROM container_events ce
			WHERE ce.kind = res.kind AND ce.resource_id = res.id
				AND ce.event IN ('exit', 'restart') AND ce.at > NOW() - INTERVAL '24 hours'
		) e ON TRUE
		WHERE res.kind = ANY($2)",
	)
	.bind(team)
	.bind(&kinds)
	.fetch_all(db)
	.await
}
