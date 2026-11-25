//! What the sampler remembers between two ticks to turn cumulative counters into rates, and
//! how each source's reading becomes figures of [`ExtSamples`].
//!
//! The memory is per server and lives in the process: after a restart of podkit the first tick
//! of each counter only stores its value, and the second one draws a point. A counter that
//! has not been seen for [`MAX_GAP`] is treated the same way, so a rate never averages over a
//! long silence.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use proto::{DbStat, Stats};

use super::ext::ExtSamples;
use super::http::{Counters, rates, sum};
use crate::ids::ServerId;

/// A previous reading older than this is not used for a rate.
const MAX_GAP: i64 = 600;
/// What the first question for events looks back, so a crash right after a restart of podkit is
/// still seen.
const FIRST_EVENT_LOOKBACK: i64 = 60;

/// Per server memory, shared by the sampler's concurrent tasks.
pub type Shared = Arc<Mutex<HashMap<ServerId, Memory>>>;

/// The previous readings of one server.
#[derive(Debug, Default)]
pub struct Memory {
	/// Per resource: its summed Traefik counters and when they were read.
	http: HashMap<(String, i64), (Counters, i64)>,
	/// Per database: cumulative operations and when they were read.
	db_ops: HashMap<i64, (i64, i64)>,
	/// Per model: cumulative (requests, tokens) served and when they were read.
	models: HashMap<i64, ((i64, i64), i64)>,
	/// The end of the last window of events that was stored.
	events_until: Option<i64>,
}

fn point<'a>(out: &'a mut ExtSamples, kind: &str, id: i64) -> &'a mut super::ext::ExtPoint {
	out.entry((kind.to_owned(), id)).or_default()
}

impl Memory {
	/// Rates and latency quantiles of every routed resource, from the counters in `stats`.
	pub fn http(&mut self, at: i64, stats: &Stats, out: &mut ExtSamples) {
		let mut by_resource: HashMap<(String, i64), Vec<&proto::HttpStats>> = HashMap::new();
		for s in &stats.http {
			if let Some(r) = &s.resource {
				by_resource
					.entry((r.kind.clone(), r.id))
					.or_default()
					.push(s);
			}
		}
		for (key, services) in by_resource {
			let now = sum(services);
			if let Some((before, was)) = self.http.get(&key)
				&& at - was <= MAX_GAP
				&& let Some(r) = rates(before, &now, (at - was) as f64)
			{
				let p = point(out, &key.0, key.1);
				p.req_rate = Some(r.req);
				p.r2xx_rate = Some(r.r2xx);
				p.r3xx_rate = Some(r.r3xx);
				p.r4xx_rate = Some(r.r4xx);
				p.r5xx_rate = Some(r.r5xx);
				p.p50_ms = r.p50_ms;
				p.p95_ms = r.p95_ms;
				p.bytes_in_rate = Some(r.bytes_in);
				p.bytes_out_rate = Some(r.bytes_out);
			}
			self.http.insert(key, (now, at));
		}
	}

	/// Connections and size as they are, operations per second from the counter's growth.
	pub fn databases(&mut self, at: i64, stats: &[DbStat], out: &mut ExtSamples) {
		for s in stats {
			let p = point(out, "database", s.id);
			p.db_conns = s.connections;
			p.db_size = s.size;
			if let Some(ops) = s.operations {
				if let Some((before, was)) = self.db_ops.get(&s.id)
					&& at > *was
					&& at - was <= MAX_GAP
					// A counter that went down is a restart of the engine: no rate this time.
					&& ops >= *before
				{
					p.db_ops_rate = Some((ops - before) as f64 / (at - was) as f64);
				}
				self.db_ops.insert(s.id, (ops, at));
			}
		}
	}

	/// Tokens and requests per second the gateway served, from the growth of each model's
	/// totals in `model_usage`. `totals` is `(model id, requests, tokens)`. Usage arrives with
	/// the status sync, so the series moves in steps of that cadence.
	pub fn models(&mut self, at: i64, totals: &[(i64, i64, i64)], out: &mut ExtSamples) {
		for &(id, requests, tokens) in totals {
			if let Some(((req_before, tok_before), was)) = self.models.get(&id)
				&& at > *was
				&& at - was <= MAX_GAP
				&& requests >= *req_before
				&& tokens >= *tok_before
			{
				let secs = (at - was) as f64;
				let p = point(out, "model", id);
				p.model_req_rate = Some((requests - req_before) as f64 / secs);
				p.tokens_rate = Some((tokens - tok_before) as f64 / secs);
			}
			self.models.insert(id, ((requests, tokens), at));
		}
	}

	/// The window of events to ask for at `now`: from where the last answer ended.
	#[must_use]
	pub fn event_window(&self, now: i64) -> (i64, i64) {
		(self.events_until.unwrap_or(now - FIRST_EVENT_LOOKBACK), now)
	}

	/// Remembers that events up to `until` are stored. The next window starts one second
	/// later, because Podman's `until` is inclusive.
	pub fn events_stored(&mut self, until: i64) {
		self.events_until = Some(until + 1);
	}
}

/// Volume bytes and GPU memory per resource, as the agent measured them.
pub fn volumes_and_vram(stats: &Stats, out: &mut ExtSamples) {
	for v in &stats.volumes {
		// The services of a stack share one row: their volumes add up.
		let p = point(out, &v.resource.kind, v.resource.id);
		p.volume_bytes = Some(
			p.volume_bytes
				.unwrap_or(0)
				.saturating_add(i64::try_from(v.used).unwrap_or(i64::MAX)),
		);
	}
	for v in &stats.vram {
		let p = point(out, &v.resource.kind, v.resource.id);
		p.vram_bytes = Some(
			p.vram_bytes
				.unwrap_or(0)
				.saturating_add(i64::try_from(v.used).unwrap_or(i64::MAX)),
		);
	}
}
