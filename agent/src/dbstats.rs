//! `POST /v1/db-stats`: connections, size, activity and memory of database containers.
//!
//! The figures come from the engine itself, through a read-only command run inside the
//! database container ([`proto::Engine::stats_cmd`]): the container's own environment holds
//! the credentials, so none travels from the control plane. Each command prints
//! `connections|size|operations|memory` on its last line and the parser below turns that into
//! a [`DbStat`].

use std::time::Duration;

use futures_util::StreamExt;
use proto::{DbStat, DbStatsTarget};

use crate::podman::Podman;

const TIMEOUT: Duration = Duration::from_secs(8);
const CONCURRENT: usize = 4;

/// Parses the last non-empty line of a stats command: four `|`-separated fields, each an
/// integer or empty. `None` when the output is not that shape (an error message, a login
/// failure).
#[must_use]
pub fn parse_line(id: i64, output: &str) -> Option<DbStat> {
	let line = output.lines().rev().find(|l| !l.trim().is_empty())?.trim();
	let fields: Vec<&str> = line.split('|').collect();
	if fields.len() != 4 {
		return None;
	}
	let number = |s: &str| -> Result<Option<i64>, ()> {
		let s = s.trim();
		if s.is_empty() {
			return Ok(None);
		}
		if let Ok(n) = s.parse::<i64>() {
			return Ok(Some(n));
		}
		// Mongo prints sizes as floats; they round down.
		#[allow(clippy::cast_possible_truncation)]
		s.parse::<f64>()
			.ok()
			.filter(|f| f.is_finite() && *f >= 0.0)
			.map(|f| Some(f as i64))
			.ok_or(())
	};
	let [connections, size, operations, memory] = [
		number(fields[0]).ok()?,
		number(fields[1]).ok()?,
		number(fields[2]).ok()?,
		number(fields[3]).ok()?,
	];
	// An all-empty line says nothing.
	if connections.is_none() && size.is_none() && operations.is_none() && memory.is_none() {
		return None;
	}
	Some(DbStat {
		id,
		connections,
		size,
		operations,
		memory,
	})
}

async fn read_one(podman: &Podman, target: &DbStatsTarget) -> Option<DbStat> {
	let cmd = target.engine.stats_cmd();
	let argv = ["sh", "-c", cmd];
	let run = podman.exec_output(&target.container, &argv);
	let (code, output) = tokio::time::timeout(TIMEOUT, run).await.ok()?.ok()?;
	if code != 0 {
		return None;
	}
	parse_line(target.id, &output)
}

/// Reads every target; a database that does not answer is left out.
pub async fn collect(podman: &Podman, targets: &[DbStatsTarget]) -> Vec<DbStat> {
	futures_util::stream::iter(targets.to_vec())
		.map(|t| async move { read_one(podman, &t).await })
		.buffer_unordered(CONCURRENT)
		.filter_map(std::future::ready)
		.collect()
		.await
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn dbstats_postgres_row_parses() {
		// `psql -Atq` of the pg_stat_database query: connections, bytes, transactions, no memory.
		let stat = parse_line(4, "3|8456719|120394|\n").unwrap();
		assert_eq!(
			stat,
			DbStat {
				id: 4,
				connections: Some(3),
				size: Some(8_456_719),
				operations: Some(120_394),
				memory: None,
			}
		);
	}

	#[test]
	fn dbstats_redis_and_mysql_rows_leave_out_what_is_cheap_to_skip() {
		let redis = parse_line(1, "2||5531|1048576\n").unwrap();
		assert_eq!(redis.size, None);
		assert_eq!(redis.memory, Some(1_048_576));
		let mysql = parse_line(2, "5|16384|9001|\n").unwrap();
		assert_eq!((mysql.connections, mysql.operations), (Some(5), Some(9001)));
	}

	#[test]
	fn dbstats_mongo_floats_round_down_and_noise_before_the_line_is_ignored() {
		let stat = parse_line(3, "MongoNetworkError retry\n7|12345.0|88|104857600\n").unwrap();
		assert_eq!(stat.size, Some(12_345));
		assert_eq!(stat.connections, Some(7));
	}

	#[test]
	fn dbstats_rejects_output_that_is_not_the_shape() {
		for bad in [
			"",
			"\n\n",
			"psql: error: connection to server failed",
			"1|2|3",
			"a|b|c|d",
			"|||",
			"1|2|3|4|5",
		] {
			assert_eq!(parse_line(1, bad), None, "{bad:?}");
		}
	}

	#[test]
	fn dbstats_commands_read_credentials_from_the_container_not_the_request() {
		use proto::Engine;
		for (engine, var) in [
			(Engine::Postgres, "$POSTGRES_USER"),
			(Engine::Mysql, "$MYSQL_ROOT_PASSWORD"),
			(Engine::Mongodb, "$MONGO_INITDB_ROOT_PASSWORD"),
			(Engine::Redis, "$REDIS_PASSWORD"),
		] {
			assert!(engine.stats_cmd().contains(var), "{engine:?}");
		}
		// The Postgres session cannot write.
		assert!(
			Engine::Postgres
				.stats_cmd()
				.contains("default_transaction_read_only=on")
		);
		assert!(Engine::Mysql.stats_cmd().contains("READ ONLY"));
	}
}
