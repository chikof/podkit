//! When a server's disk fills, at the rate it filled lately: a least-squares line through
//! the disk-used samples of the last week ([`estimate`]), read from the rows the sampler
//! already stores ([`disk`]). Shown beside the disk figure on a server's Overview.

use sqlx::PgPool;
use time::OffsetDateTime;

use super::{Range, ServerPoint};
use crate::ids::ServerId;

/// Fewer samples than this say too little about a trend.
pub const MIN_SAMPLES: usize = 12;
/// The samples must cover at least this long (seconds): a day, so a nightly job that fills
/// and empties the disk does not read as a trend.
pub const MIN_SPAN_SECS: i64 = 24 * 3600;
/// Slower than this (bytes a day) is flat: logs and caches that barely move.
const FLAT_BYTES_PER_DAY: f64 = 1024.0 * 1024.0;
/// Further out than this (seconds) is not a forecast anyone can act on: it reads as flat.
const HORIZON_SECS: f64 = 365.0 * 86400.0;

/// What the recent samples say about a server's disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskForecast {
	/// Full in about this many seconds (0 when it already is).
	FullIn(i64),
	/// Flat or shrinking.
	NotFilling,
	/// Too few samples, or too short a stretch of them.
	NotEnoughData,
}

impl DiskForecast {
	/// The line under the disk figure.
	#[must_use]
	pub fn label(self) -> String {
		match self {
			Self::FullIn(secs) => {
				let hours = secs / 3600;
				let days = secs / 86400;
				match (days, hours) {
					(_, 0) => "Full within the hour at this rate".to_owned(),
					(0, 1) => "Full in about an hour at this rate".to_owned(),
					(0, h) => format!("Full in about {h} hours at this rate"),
					(1, _) => "Full in about a day at this rate".to_owned(),
					(d, _) => format!("Full in about {d} days at this rate"),
				}
			}
			Self::NotFilling => "Not filling up".to_owned(),
			Self::NotEnoughData => "Not enough data yet".to_owned(),
		}
	}
}

/// Fits `used = a + slope * at` through `points` (oldest first, `at` in unix seconds) and
/// says when the line reaches the newest sample's `disk_total`. Samples without a disk
/// (`disk_total` 0) are ignored. Pure: [`disk`] feeds it the stored series.
#[must_use]
pub fn estimate(points: &[ServerPoint]) -> DiskForecast {
	let samples: Vec<&ServerPoint> = points.iter().filter(|p| p.disk_total > 0).collect();
	let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
		return DiskForecast::NotEnoughData;
	};
	if samples.len() < MIN_SAMPLES || last.at - first.at < MIN_SPAN_SECS {
		return DiskForecast::NotEnoughData;
	}
	// Seconds from the first sample keep the sums small enough for an f64.
	let n = samples.len() as f64;
	let xs = || samples.iter().map(|p| (p.at - first.at) as f64);
	let ys = || samples.iter().map(|p| p.disk_used as f64);
	let mean_x = xs().sum::<f64>() / n;
	let mean_y = ys().sum::<f64>() / n;
	let (cov, var) = xs().zip(ys()).fold((0.0, 0.0), |(cov, var), (x, y)| {
		(
			cov + (x - mean_x) * (y - mean_y),
			var + (x - mean_x).powi(2),
		)
	});
	if var <= 0.0 {
		return DiskForecast::NotEnoughData;
	}
	let slope = cov / var;
	if slope * 86400.0 < FLAT_BYTES_PER_DAY {
		return DiskForecast::NotFilling;
	}
	// From the line's value now rather than the last sample, so one noisy reading does not
	// move the answer.
	let used_now = mean_y + slope * ((last.at - first.at) as f64 - mean_x);
	let left = (last.disk_total as f64 - used_now).max(0.0);
	let secs = left / slope;
	if secs > HORIZON_SECS {
		return DiskForecast::NotFilling;
	}
	// `secs` is at most the horizon here, well inside an i64.
	#[allow(clippy::cast_possible_truncation)]
	DiskForecast::FullIn(secs.round() as i64)
}

/// The forecast of `server`'s disk from the last week of samples.
///
/// # Errors
/// The database rejects the read.
pub async fn disk(
	db: &PgPool,
	server: ServerId,
	now: OffsetDateTime,
) -> Result<DiskForecast, sqlx::Error> {
	let points = super::server_series(db, server, Range::Week, now).await?;
	Ok(estimate(&points))
}

#[cfg(test)]
mod tests {
	use super::*;

	const GB: i64 = 1024 * 1024 * 1024;
	const HOUR: i64 = 3600;

	fn point(at: i64, used: i64, total: i64) -> ServerPoint {
		ServerPoint {
			at,
			cpu: None,
			mem_used: 0,
			mem_total: 0,
			disk_used: used,
			disk_total: total,
			net_rx: 0,
			net_tx: 0,
			gpu_util: None,
			vram_used: None,
		}
	}

	/// One sample an hour for `hours`, `used` given by the hour.
	fn series(hours: i64, total: i64, used: impl Fn(i64) -> i64) -> Vec<ServerPoint> {
		(0..=hours)
			.map(|h| point(1_700_000_000 + h * HOUR, used(h), total))
			.collect()
	}

	#[test]
	fn a_steady_climb_fills_when_the_line_says() {
		// 1 GB a day, 50 GB used of 60 at the end: 10 days left.
		let points = series(48, 60 * GB, |h| 48 * GB + h * GB / 24);
		let DiskForecast::FullIn(secs) = estimate(&points) else {
			panic!("expected a forecast");
		};
		assert!((secs - 10 * 86400).abs() < 60, "{secs}");
		assert_eq!(
			estimate(&points).label(),
			"Full in about 10 days at this rate"
		);
	}

	#[test]
	fn noise_around_the_trend_does_not_change_the_answer_much() {
		let points = series(72, 100 * GB, |h| {
			let wobble = if h % 2 == 0 { GB / 2 } else { -GB / 2 };
			50 * GB + h * 2 * GB / 24 + wobble
		});
		let DiskForecast::FullIn(secs) = estimate(&points) else {
			panic!("expected a forecast");
		};
		// The line ends at 56 GB: 44 GB left at 2 GB a day.
		assert!((secs - 22 * 86400).abs() < HOUR, "{secs}");
	}

	#[test]
	fn flat_and_shrinking_disks_are_not_filling() {
		assert_eq!(
			estimate(&series(48, 100 * GB, |_| 40 * GB)),
			DiskForecast::NotFilling
		);
		assert_eq!(
			estimate(&series(48, 100 * GB, |h| 40 * GB - h * GB / 10)),
			DiskForecast::NotFilling
		);
		// A few KB a day is flat too.
		assert_eq!(
			estimate(&series(48, 100 * GB, |h| 40 * GB + h * 100)),
			DiskForecast::NotFilling
		);
		// So is a climb that would take years.
		assert_eq!(
			estimate(&series(48, 1000 * GB, |h| 10 * GB + h * (GB / 1000))),
			DiskForecast::NotFilling
		);
	}

	#[test]
	fn too_few_or_too_short_is_not_enough_data() {
		assert_eq!(estimate(&[]), DiskForecast::NotEnoughData);
		// Every 2 hours for a day: enough span, too few samples.
		let sparse: Vec<_> = (0..=10)
			.map(|i| point(i * 2 * HOUR, GB + i * GB, 100 * GB))
			.collect();
		assert_eq!(estimate(&sparse), DiskForecast::NotEnoughData);
		// Plenty of samples, half a day.
		let short: Vec<_> = (0..=72)
			.map(|i| point(i * 600, GB + i * GB, 100 * GB))
			.collect();
		assert_eq!(estimate(&short), DiskForecast::NotEnoughData);
		// A host that reports no disk records 0 of 0.
		assert_eq!(estimate(&series(48, 0, |_| 0)), DiskForecast::NotEnoughData);
	}

	#[test]
	fn a_full_disk_still_climbing_is_full_now() {
		let points = series(48, 88 * GB, |h| 40 * GB + h * GB);
		assert_eq!(estimate(&points), DiskForecast::FullIn(0));
		assert_eq!(
			DiskForecast::FullIn(0).label(),
			"Full within the hour at this rate"
		);
	}

	#[test]
	fn labels_round_down_to_hours_and_days() {
		assert_eq!(
			DiskForecast::FullIn(90 * 60).label(),
			"Full in about an hour at this rate"
		);
		assert_eq!(
			DiskForecast::FullIn(5 * HOUR + 10).label(),
			"Full in about 5 hours at this rate"
		);
		assert_eq!(
			DiskForecast::FullIn(30 * HOUR).label(),
			"Full in about a day at this rate"
		);
		assert_eq!(
			DiskForecast::FullIn(9 * 86400 + 100).label(),
			"Full in about 9 days at this rate"
		);
		assert_eq!(DiskForecast::NotFilling.label(), "Not filling up");
		assert_eq!(DiskForecast::NotEnoughData.label(), "Not enough data yet");
	}
}
