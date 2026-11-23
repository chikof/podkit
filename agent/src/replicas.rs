//! Replicas of one service on this server: container names, and the order in which a
//! rolling update starts the new copies and retires the old ones.
//!
//! Everything here is pure; `deploy` does the container work and follows the plan.

/// Most replicas of one service.
pub const MAX_REPLICAS: u32 = 10;

/// The container name of replica `index` (0-based). The first keeps the name a single
/// container always had, so a service scaled down to one is not renamed.
#[must_use]
pub fn name(service: &str, instance_id: i64, index: usize) -> String {
	if index == 0 {
		format!("podkit-{service}-{instance_id}")
	} else {
		format!("podkit-{service}-{instance_id}-r{}", index + 1)
	}
}

/// Whether `rest` (a container name after `podkit-{service}-`) is an instance of the
/// service: the instance id, then `-r{n}` for a replica after the first.
#[must_use]
pub fn is_instance(rest: &str) -> bool {
	order_key(rest).is_some()
}

/// `(instance id, replica number)` of what follows `podkit-{service}-`, for ordering the
/// old containers oldest first. `None` when it is not an instance name.
#[must_use]
pub fn order_key(rest: &str) -> Option<(u64, u32)> {
	let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
	match rest.split_once("-r") {
		None if digits(rest) => Some((rest.parse().ok()?, 1)),
		Some((id, n)) if digits(id) && digits(n) => Some((id.parse().ok()?, n.parse().ok()?)),
		_ => None,
	}
}

/// One round of a rolling update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
	/// Old containers taken away before the new ones start (`max_unavailable`).
	pub retire_before: usize,
	/// The replicas (0-based) started in this round, then waited on until healthy.
	pub start: Vec<usize>,
	/// Old containers retired once the new ones are healthy (`max_surge`).
	pub retire_after: usize,
}

/// The rounds that take `old` running containers to `count` new ones.
///
/// With a surge, a round starts up to `surge` new containers next to the old ones and
/// retires as many old ones once they are healthy, so capacity never drops. Without one, a
/// round retires up to `unavailable` old containers first, then starts their replacements.
/// A service with nothing running yet starts everything in one round. The old containers
/// beyond the new count go with the last round, so a service that stays at one container
/// swaps exactly as it always did.
#[must_use]
pub fn plan(old: usize, count: usize, surge: usize, unavailable: usize) -> Vec<Step> {
	if old == 0 {
		return vec![Step {
			retire_before: 0,
			start: (0..count).collect(),
			retire_after: 0,
		}];
	}
	let batch = if surge > 0 { surge } else { unavailable.max(1) };
	let mut steps = Vec::new();
	let (mut next, mut left) = (0, old);
	while next < count {
		let k = batch.min(count - next);
		let retired = k.min(left);
		left -= retired;
		steps.push(Step {
			retire_before: if surge > 0 { 0 } else { retired },
			start: (next..next + k).collect(),
			retire_after: if surge > 0 { retired } else { 0 },
		});
		next += k;
	}
	if left > 0
		&& let Some(last) = steps.last_mut()
	{
		// Containers beyond the new count go with the last round, after its replacements
		// are healthy.
		last.retire_after += left;
	}
	steps
}

/// Refuses a replica setting the agent cannot run.
///
/// # Errors
/// A message for the deploy log.
pub fn validate(replicas: &proto::Replicas) -> Result<(), String> {
	if !(1..=MAX_REPLICAS).contains(&replicas.count) {
		return Err(format!("replicas must be between 1 and {MAX_REPLICAS}"));
	}
	if replicas.max_surge == 0 && replicas.max_unavailable == 0 {
		return Err("a rolling update needs a surge or an unavailable replica".to_string());
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn replicas_names_are_distinct_and_the_first_keeps_its_old_name() {
		assert_eq!(name("app-1", 9, 0), "podkit-app-1-9");
		assert_eq!(name("app-1", 9, 1), "podkit-app-1-9-r2");
		assert_eq!(name("app-1", 9, 9), "podkit-app-1-9-r10");
		let all: std::collections::HashSet<_> = (0..10).map(|i| name("app-1", 9, i)).collect();
		assert_eq!(all.len(), 10);
	}

	#[test]
	fn replicas_instance_names_are_told_from_other_services() {
		for rest in ["9", "12", "9-r2", "9-r10"] {
			assert!(is_instance(rest), "{rest}");
		}
		// `podkit-app-1-` also prefixes `podkit-app-1-web-3` of a stack: not an instance.
		for rest in ["", "web-3", "9-", "9-r", "9-rx", "-r2", "9-r2-r3"] {
			assert!(!is_instance(rest), "{rest}");
		}
		assert_eq!(order_key("8"), Some((8, 1)));
		assert_eq!(order_key("8-r3"), Some((8, 3)));
		assert!(order_key("8-r3") > order_key("8"));
		assert!(order_key("9") > order_key("8-r3"));
	}

	fn shape(steps: &[Step]) -> Vec<(usize, usize, usize)> {
		steps
			.iter()
			.map(|s| (s.retire_before, s.start.len(), s.retire_after))
			.collect()
	}

	#[test]
	fn replicas_first_deploy_starts_everything_at_once() {
		let steps = plan(0, 3, 1, 0);
		assert_eq!(steps.len(), 1);
		assert_eq!(steps[0].start, [0, 1, 2]);
	}

	#[test]
	fn replicas_rolling_order_surges_one_then_retires_one() {
		let steps = plan(3, 3, 1, 0);
		assert_eq!(shape(&steps), [(0, 1, 1), (0, 1, 1), (0, 1, 1)]);
		let started: Vec<usize> = steps.iter().flat_map(|s| s.start.clone()).collect();
		assert_eq!(started, [0, 1, 2]);
	}

	#[test]
	fn replicas_rolling_with_a_bigger_surge_goes_in_batches() {
		assert_eq!(shape(&plan(4, 4, 2, 0)), [(0, 2, 2), (0, 2, 2)]);
		// A surge wider than the service is one round.
		assert_eq!(shape(&plan(2, 2, 5, 0)), [(0, 2, 2)]);
	}

	#[test]
	fn replicas_rolling_without_surge_retires_before_it_starts() {
		let steps = plan(3, 3, 0, 1);
		assert_eq!(shape(&steps), [(1, 1, 0), (1, 1, 0), (1, 1, 0)]);
		assert_eq!(shape(&plan(4, 4, 0, 2)), [(2, 2, 0), (2, 2, 0)]);
	}

	#[test]
	fn replicas_scaling_keeps_every_old_container_accounted_for() {
		for (old, count, surge, unavailable) in [
			(5, 3, 1, 0),
			(2, 5, 1, 0),
			(5, 3, 0, 1),
			(1, 10, 3, 0),
			(10, 1, 2, 0),
		] {
			let steps = plan(old, count, surge, unavailable);
			let retired: usize = steps.iter().map(|s| s.retire_before + s.retire_after).sum();
			let started: usize = steps.iter().map(|s| s.start.len()).sum();
			assert_eq!(retired, old, "{old}->{count}");
			assert_eq!(started, count, "{old}->{count}");
		}
		// Scaling down: the containers beyond the new count go with the last round.
		let last = plan(5, 3, 1, 0).pop().unwrap();
		assert_eq!(last.start.len(), 1);
		assert_eq!(last.retire_after, 3);
	}

	#[test]
	fn replicas_surge_never_drops_below_the_wanted_capacity() {
		// With a surge, running containers (old left + new healthy) never fall below
		// min(old, count) after any round.
		for (old, count, surge) in [(3, 3, 1), (4, 6, 2), (6, 4, 2), (2, 2, 1)] {
			let (mut live_old, mut live_new) = (old, 0);
			for step in plan(old, count, surge, 0) {
				live_new += step.start.len();
				live_old -= step.retire_after;
				assert!(live_old + live_new >= old.min(count), "{old}->{count}");
			}
		}
	}

	#[test]
	fn replicas_settings_are_refused_outside_their_limits() {
		let ok = proto::Replicas {
			count: 3,
			max_surge: 1,
			max_unavailable: 0,
			sticky: false,
		};
		assert!(validate(&ok).is_ok());
		assert!(validate(&proto::Replicas { count: 0, ..ok }).is_err());
		assert!(validate(&proto::Replicas { count: 11, ..ok }).is_err());
		assert!(validate(&proto::Replicas { max_surge: 0, ..ok }).is_err());
	}
}
