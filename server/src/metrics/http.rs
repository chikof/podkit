//! HTTP series: the difference of two readings of Traefik's cumulative counters, as rates and
//! latency quantiles.
//!
//! The agent hands over counters as Traefik keeps them (see `agent/src/prometheus.rs`). A
//! counter only grows until Traefik restarts, so a rate is `(now - before) / seconds`, and a
//! reading smaller than the one before means a restart: the new value counts from zero.
//! Latency comes from the histogram: the growth of each cumulative bucket over the interval is
//! the histogram of the requests of that interval, and a quantile is read off it by linear
//! interpolation inside the bucket, as Prometheus's `histogram_quantile` does
//! (<https://prometheus.io/docs/prometheus/latest/querying/functions/#histogram_quantile>).

use std::collections::BTreeMap;

use proto::HttpStats;

/// Counters of one resource: every routed service of it, summed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Counters {
	pub r2xx: f64,
	pub r3xx: f64,
	pub r4xx: f64,
	pub r5xx: f64,
	pub bytes_in: f64,
	pub bytes_out: f64,
	/// Cumulative buckets, `le` ascending; `None` is `+Inf`.
	pub buckets: Vec<(Option<f64>, f64)>,
}

/// What a resource served per second over one interval.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rates {
	pub req: f64,
	pub r2xx: f64,
	pub r3xx: f64,
	pub r4xx: f64,
	pub r5xx: f64,
	pub bytes_in: f64,
	pub bytes_out: f64,
	/// Milliseconds; `None` when nothing was served in the interval.
	pub p50_ms: Option<f64>,
	pub p95_ms: Option<f64>,
}

/// Sums the services of one resource.
#[must_use]
pub fn sum<'a>(services: impl IntoIterator<Item = &'a HttpStats>) -> Counters {
	let mut total = Counters::default();
	// `le` as raw bits keeps `+Inf` (None) and equal bounds together in a sorted map.
	let mut buckets: BTreeMap<(bool, u64), (Option<f64>, f64)> = BTreeMap::new();
	for s in services {
		total.r2xx += s.r2xx;
		total.r3xx += s.r3xx;
		total.r4xx += s.r4xx;
		total.r5xx += s.r5xx;
		total.bytes_in += s.bytes_in;
		total.bytes_out += s.bytes_out;
		for b in &s.buckets {
			// Finite bounds are positive, so their bits sort like the numbers.
			let key = (b.le.is_none(), b.le.map_or(0, f64::to_bits));
			buckets.entry(key).or_insert((b.le, 0.0)).1 += b.count;
		}
	}
	total.buckets = buckets.into_values().collect();
	total
}

/// The growth of `now` over `before` per second. `None` when no time passed. A counter that
/// went down is a restart of Traefik: its new value is the growth.
#[must_use]
pub fn rates(before: &Counters, now: &Counters, seconds: f64) -> Option<Rates> {
	if seconds <= 0.0 || !seconds.is_finite() {
		return None;
	}
	let grew = |b: f64, n: f64| if n >= b { n - b } else { n };
	// One counter going down means the whole process restarted; use every new value as is.
	let restarted = now.r2xx < before.r2xx
		|| now.r3xx < before.r3xx
		|| now.r4xx < before.r4xx
		|| now.r5xx < before.r5xx
		|| now.bytes_in < before.bytes_in;
	let delta = |b: f64, n: f64| if restarted { n } else { grew(b, n) };
	let (d2, d3, d4, d5) = (
		delta(before.r2xx, now.r2xx),
		delta(before.r3xx, now.r3xx),
		delta(before.r4xx, now.r4xx),
		delta(before.r5xx, now.r5xx),
	);
	let buckets: Vec<(Option<f64>, f64)> = now
		.buckets
		.iter()
		.map(|(le, count)| {
			let before = before
				.buckets
				.iter()
				.find(|(l, _)| l == le)
				.map_or(0.0, |(_, c)| *c);
			(
				*le,
				if restarted {
					*count
				} else {
					grew(before, *count)
				},
			)
		})
		.collect();
	Some(Rates {
		req: (d2 + d3 + d4 + d5) / seconds,
		r2xx: d2 / seconds,
		r3xx: d3 / seconds,
		r4xx: d4 / seconds,
		r5xx: d5 / seconds,
		bytes_in: delta(before.bytes_in, now.bytes_in) / seconds,
		bytes_out: delta(before.bytes_out, now.bytes_out) / seconds,
		p50_ms: quantile(0.5, &buckets).map(|s| s * 1000.0),
		p95_ms: quantile(0.95, &buckets).map(|s| s * 1000.0),
	})
}

/// The `q` quantile (0 to 1) of a cumulative histogram, in the buckets' unit. `None` for an
/// empty histogram. Inside a bucket the observations are taken as evenly spread; a quantile
/// that falls in the `+Inf` bucket is the largest finite bound, the best the buckets know.
#[must_use]
pub fn quantile(q: f64, buckets: &[(Option<f64>, f64)]) -> Option<f64> {
	let total = buckets.last()?.1;
	if total <= 0.0 || !(0.0..=1.0).contains(&q) {
		return None;
	}
	let rank = q * total;
	let mut lower = 0.0;
	let mut seen = 0.0;
	for (le, count) in buckets {
		if *count >= rank {
			let Some(upper) = *le else {
				// +Inf: nothing finite to interpolate to.
				return (lower > 0.0).then_some(lower);
			};
			let in_bucket = count - seen;
			if in_bucket <= 0.0 {
				return Some(upper);
			}
			return Some(lower + (upper - lower) * (rank - seen) / in_bucket);
		}
		if let Some(le) = le {
			lower = *le;
		}
		seen = *count;
	}
	None
}

#[cfg(test)]
mod tests {
	use proto::{HistogramBucket, ResourceRef};

	use super::*;

	fn b(le: Option<f64>, count: f64) -> HistogramBucket {
		HistogramBucket { le, count }
	}

	fn service(r2: f64, r4: f64, buckets: Vec<HistogramBucket>) -> HttpStats {
		HttpStats {
			resource: Some(ResourceRef {
				kind: "application".to_owned(),
				id: 1,
				service: None,
			}),
			r2xx: r2,
			r4xx: r4,
			bytes_in: r2 * 10.0,
			bytes_out: r2 * 100.0,
			buckets,
			..Default::default()
		}
	}

	#[test]
	fn http_metrics_quantiles_interpolate_inside_the_bucket() {
		// 100 requests: 50 under 100 ms, 40 more under 500 ms, 10 more under 1 s.
		let h = [
			(Some(0.1), 50.0),
			(Some(0.5), 90.0),
			(Some(1.0), 100.0),
			(None, 100.0),
		];
		// The median is the top of the first bucket: rank 50 is reached at 0.1 s.
		assert!((quantile(0.5, &h).unwrap() - 0.1).abs() < 1e-9);
		// p95: rank 95 falls halfway through the (0.5, 1.0] bucket: 5 of its 10.
		assert!((quantile(0.95, &h).unwrap() - 0.75).abs() < 1e-9);
		// p25: halfway through the first bucket, which starts at 0.
		assert!((quantile(0.25, &h).unwrap() - 0.05).abs() < 1e-9);
	}

	#[test]
	fn http_metrics_quantile_edge_cases() {
		assert_eq!(quantile(0.5, &[]), None);
		assert_eq!(quantile(0.5, &[(Some(1.0), 0.0), (None, 0.0)]), None);
		assert_eq!(quantile(1.5, &[(None, 3.0)]), None);
		// Everything slower than the last finite bucket: the bound is the best answer.
		let slow = [(Some(1.0), 0.0), (Some(10.0), 0.0), (None, 5.0)];
		assert_eq!(quantile(0.95, &slow), Some(10.0));
		// Only an +Inf bucket: no bound at all.
		assert_eq!(quantile(0.5, &[(None, 5.0)]), None);
	}

	#[test]
	fn http_metrics_rates_are_growth_per_second() {
		let before = sum([&service(
			100.0,
			10.0,
			vec![b(Some(0.1), 100.0), b(Some(1.0), 110.0), b(None, 110.0)],
		)]);
		let now = sum([&service(
			190.0,
			20.0,
			vec![b(Some(0.1), 190.0), b(Some(1.0), 210.0), b(None, 210.0)],
		)]);
		let r = rates(&before, &now, 30.0).unwrap();
		// 90 more 2xx and 10 more 4xx in 30 seconds.
		assert!((r.r2xx - 3.0).abs() < 1e-9);
		assert!((r.r4xx - 10.0 / 30.0).abs() < 1e-9);
		assert!((r.req - 100.0 / 30.0).abs() < 1e-9);
		assert!((r.bytes_out - 90.0 * 100.0 / 30.0).abs() < 1e-9);
		// The 100 new requests: 90 under 100 ms, 10 more under 1 s. The median is rank 50 of the
		// first bucket's 90: 100 ms * 50 / 90.
		assert!((r.p50_ms.unwrap() - 100.0 * 50.0 / 90.0).abs() < 1e-6);
		assert!(r.p95_ms.unwrap() > 100.0 && r.p95_ms.unwrap() <= 1000.0);
	}

	#[test]
	fn http_metrics_a_restart_counts_from_zero_and_a_quiet_interval_has_no_latency() {
		let before = sum([&service(
			1000.0,
			0.0,
			vec![b(Some(1.0), 1000.0), b(None, 1000.0)],
		)]);
		let after_restart = sum([&service(30.0, 0.0, vec![b(Some(1.0), 30.0), b(None, 30.0)])]);
		let r = rates(&before, &after_restart, 10.0).unwrap();
		assert!((r.r2xx - 3.0).abs() < 1e-9);
		assert!(r.p50_ms.is_some());
		let quiet = rates(&before, &before, 10.0).unwrap();
		assert_eq!((quiet.req, quiet.p50_ms, quiet.p95_ms), (0.0, None, None));
		assert_eq!(rates(&before, &before, 0.0), None);
	}

	#[test]
	fn http_metrics_a_stacks_services_add_up() {
		let a = service(10.0, 0.0, vec![b(Some(0.1), 10.0), b(None, 10.0)]);
		let c = service(5.0, 1.0, vec![b(Some(0.1), 4.0), b(None, 6.0)]);
		let total = sum([&a, &c]);
		assert_eq!((total.r2xx, total.r4xx), (15.0, 1.0));
		assert_eq!(total.buckets, [(Some(0.1), 14.0), (None, 16.0)]);
	}
}
