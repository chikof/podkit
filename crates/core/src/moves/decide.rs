//! The decisions of a move, as pure functions over what was looked up: whether an
//! application can move to a server, how ([`Method`]), what it costs in downtime, what the
//! owner has to do afterwards, and in which order a drain moves things.
//!
//! Nothing here reads the database or talks to an agent, so every rule is tested on its own
//! (`cargo test -p podkit-core -- moves::decide`).

use serde::{Deserialize, Serialize};

/// How an application gets to the other server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "varchar", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum Method {
	/// The running image is copied to the target and started there through the normal
	/// deploy path; once it is up the application points at the target and the old
	/// container is removed. The old one serves until then.
	Redeploy,
	/// The application has volumes: it is stopped, each volume is archived through a backup
	/// destination and restored on the target, and it is started there.
	StopCopy,
	/// The old server cannot be reached, so nothing can be copied from it: the target builds
	/// the branch again (git) or pulls the image again (registry image).
	Rebuild,
}

impl Method {
	/// The stored and API form.
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Redeploy => "redeploy",
			Self::StopCopy => "stop_copy",
			Self::Rebuild => "rebuild",
		}
	}

	/// The method in a few words, for the dialog.
	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::Redeploy => "Redeploy the running image",
			Self::StopCopy => "Stop, copy the volumes, start",
			Self::Rebuild => "Build or pull again on the target",
		}
	}

	/// What the application's users notice, when it is running.
	#[must_use]
	pub const fn downtime(self) -> &'static str {
		match self {
			Self::Redeploy => {
				"Next to none: the old container serves until the new one is up. Clients that use the generated address or a domain whose DNS still points at the old server reach the old container until it is removed."
			}
			Self::StopCopy => {
				"Down from the stop until it starts on the target: the time it takes to archive, upload, download and unpack its volumes, plus a start."
			}
			Self::Rebuild => {
				"Down until it runs on the target: the old server is unreachable, so it is probably down already. Counts a build (git) or a pull (image)."
			}
		}
	}
}

/// What podkit knows about one end of a move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct End {
	pub name: String,
	/// The agent answers (`servers.status = active`).
	pub active: bool,
	/// The server is being drained.
	pub draining: bool,
}

/// Everything a move decision looks at, gathered by the service.
#[derive(Debug, Clone, Default)]
#[allow(
	clippy::struct_excessive_bools,
	reason = "one flag per thing that ties an application to its server"
)]
pub struct Facts {
	pub app_name: String,
	pub slug: String,
	/// `None` when the application's server could not be read.
	pub from: Option<End>,
	pub from_id: i64,
	/// `None` when no target was picked yet: the target's own checks are left out.
	pub to: Option<End>,
	pub to_id: Option<i64>,
	/// Another application on the target has the same slug (`(server_id, slug)` is unique).
	pub slug_taken_on_target: bool,
	/// A deployment is live now.
	pub running: bool,
	/// Some deployment produced an image, live or not.
	pub has_image: bool,
	/// Named volumes (`volumes` rows).
	pub volumes: usize,
	/// The caller picked a backup destination for the volume data.
	pub backup_target: bool,
	/// A move of this application is queued or running.
	pub moving: bool,
	/// A deploy of it is queued, building or deploying.
	pub deploying: bool,
	/// User networks of the source server it is attached to.
	pub user_networks: Vec<String>,
	/// It is linked to a model through the server's gateway.
	pub model_links: bool,
	/// It is an MCP server, or linked to one.
	pub mcp: bool,
	/// It has a Tailscale sidecar.
	pub tailscale: bool,
	/// It is the application of a pull request preview.
	pub preview: bool,
	/// Custom domains: their DNS must point at the target afterwards.
	pub domains: Vec<String>,
	/// The generated hostname, before and after.
	pub hostname_from: Option<String>,
	pub hostname_to: Option<String>,
	/// Resources of the same environment that stay on the source server, which it reaches
	/// by name on the environment network there (`kind name`).
	pub peers: Vec<String>,
}

/// What a move would do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verdict {
	pub method: Method,
	/// Why it cannot move; empty when it can.
	pub refusals: Vec<String>,
	/// What changes for the owner, and what they must do afterwards.
	pub warnings: Vec<String>,
	/// What the users of the application notice.
	pub downtime: String,
	/// A deployment is started on the target: only when something runs now.
	pub starts: bool,
}

impl Verdict {
	#[must_use]
	pub fn movable(&self) -> bool {
		self.refusals.is_empty()
	}
}

/// The method alone: volumes force a stop and copy; an unreachable source leaves only a
/// fresh build or pull.
#[must_use]
pub fn method(source_reachable: bool, volumes: usize) -> Method {
	match (source_reachable, volumes) {
		(_, v) if v > 0 => Method::StopCopy,
		(true, _) => Method::Redeploy,
		(false, _) => Method::Rebuild,
	}
}

/// Decides whether and how the application in `f` moves.
#[must_use]
#[allow(
	clippy::too_many_lines,
	reason = "one rule after another, read top to bottom"
)]
pub fn decide(f: &Facts) -> Verdict {
	let source_reachable = f.from.as_ref().is_some_and(|e| e.active);
	let method = method(source_reachable, f.volumes);
	let mut refusals = Vec::new();
	let mut warnings = Vec::new();
	let from_name = f
		.from
		.as_ref()
		.map_or_else(|| "its server".to_owned(), |e| e.name.clone());

	// The target.
	if let Some(to) = &f.to {
		if f.to_id == Some(f.from_id) {
			refusals.push(format!("{} already runs on {}.", f.app_name, to.name));
		} else {
			if !to.active {
				refusals.push(format!(
					"{} is not ready: its agent does not answer. Bootstrap it first.",
					to.name
				));
			}
			if to.draining {
				refusals.push(format!(
					"{} is being drained and takes nothing new. Pick another server.",
					to.name
				));
			}
			if f.slug_taken_on_target {
				refusals.push(format!(
					"Another application on {} is called `{}`. Rename one of them first.",
					to.name, f.slug
				));
			}
		}
	}

	// The application itself.
	if f.moving {
		refusals.push(format!("{} is already being moved.", f.app_name));
	}
	if f.deploying {
		refusals.push(format!(
			"A deploy of {} is in progress. Move it once the deploy has finished.",
			f.app_name
		));
	}
	if f.preview {
		refusals.push(
			"It is a pull request preview: previews are made and removed with their pull request, not moved."
				.to_owned(),
		);
	}
	if f.tailscale {
		refusals.push(
			"It has a Tailscale sidecar, which is tied to this server's node. Take it off the tailnet, move it, and put it on again."
				.to_owned(),
		);
	}
	if f.model_links {
		refusals.push(format!(
			"It is linked to models through the gateway of {from_name}. Unlink the models, move it, and link them again."
		));
	}
	if f.mcp {
		refusals.push(format!(
			"It is an MCP server or uses one through the gateway of {from_name}, which is tied to that server."
		));
	}
	if !f.user_networks.is_empty() {
		refusals.push(format!(
			"It is attached to {} on {from_name}, which only exist there. Detach it first.",
			list("network", &f.user_networks)
		));
	}
	if f.volumes > 0 {
		if !source_reachable {
			refusals.push(format!(
				"Its volume data is on {from_name}, which does not answer, so it cannot be copied. Restore a volume backup on the target instead."
			));
		} else if !f.backup_target {
			refusals.push(format!(
				"It has {} named volume{}; their data is copied through a backup destination. Pick one (add one in Settings, Backups).",
				f.volumes,
				if f.volumes == 1 { "" } else { "s" }
			));
		}
	}

	// What the owner has to know.
	let starts = f.running;
	if !f.running {
		warnings.push(if f.has_image && method != Method::Rebuild {
			"Nothing runs now: its newest image is copied over and it stays stopped there until you start it.".to_owned()
		} else {
			"Nothing runs now: it is only moved, and its next deploy builds or pulls on the new server.".to_owned()
		});
	}
	if method == Method::Rebuild {
		warnings.push(format!(
			"{from_name} does not answer: the image cannot be copied, so the target builds the branch again (git) or pulls the image reference again (registry). The result may differ from what ran. If {from_name} comes back, remove the old container there."
		));
	}
	if let (Some(old), Some(new)) = (&f.hostname_from, &f.hostname_to)
		&& old != new
	{
		warnings.push(format!(
			"The generated address changes from {old} to {new}."
		));
	}
	if !f.domains.is_empty() {
		let to =
			f.to.as_ref()
				.map_or_else(|| "the new server".to_owned(), |e| e.name.clone());
		warnings.push(format!(
			"Point the DNS of {} at {to}. Until it changes, visitors reach the old server.",
			f.domains.join(", ")
		));
	}
	if !f.peers.is_empty() {
		warnings.push(format!(
			"Its environment on {from_name} also has {}, which it can reach by name there. Networks do not span servers: if it talks to them, move them too.",
			f.peers.join(", ")
		));
	}
	if f.volumes > 0 && source_reachable {
		warnings.push(format!(
			"Its volumes stay on {from_name} as they were, after the copy, until you remove them there."
		));
	}
	if f.has_image {
		warnings.push(format!(
			"Older releases stay on {from_name}: rolling back to one of them after the move needs that image on the new server, so redeploy instead."
		));
	}

	let downtime = if f.running {
		method.downtime().to_owned()
	} else {
		"None: nothing runs now.".to_owned()
	};
	Verdict {
		method,
		refusals,
		warnings,
		downtime,
		starts,
	}
}

/// `network a, b` or `networks a, b and c`.
fn list(noun: &str, names: &[String]) -> String {
	let plural = if names.len() == 1 { "" } else { "s" };
	let joined = match names {
		[] => String::new(),
		[one] => one.clone(),
		[rest @ .., last] => format!("{} and {last}", rest.join(", ")),
	};
	format!("the {noun}{plural} {joined}")
}

/// The rank of an application in a drain: quick moves first, the ones that stop it last,
/// and what cannot move at the end. Within a rank, by name.
#[must_use]
pub fn rank(verdict: &Verdict) -> u8 {
	if !verdict.movable() {
		return 3;
	}
	match verdict.method {
		Method::Redeploy => 0,
		Method::Rebuild => 1,
		Method::StopCopy => 2,
	}
}

/// Sorts drain items by [`rank`] and then by name, stably.
pub fn order<T>(
	items: &mut [T],
	verdict: impl Fn(&T) -> Option<&Verdict>,
	name: impl Fn(&T) -> &str,
) {
	items.sort_by(|a, b| {
		let ra = verdict(a).map_or(4, rank);
		let rb = verdict(b).map_or(4, rank);
		ra.cmp(&rb).then_with(|| name(a).cmp(name(b)))
	});
}

#[cfg(test)]
mod tests {
	use super::*;

	fn end(name: &str) -> End {
		End {
			name: name.to_owned(),
			active: true,
			draining: false,
		}
	}

	/// A running stateless application on node-01, moving to node-02.
	fn facts() -> Facts {
		Facts {
			app_name: "web".to_owned(),
			slug: "web".to_owned(),
			from: Some(end("node-01")),
			from_id: 1,
			to: Some(end("node-02")),
			to_id: Some(2),
			running: true,
			has_image: true,
			hostname_from: Some("web.10-0-0-1.sslip.io".to_owned()),
			hostname_to: Some("web.10-0-0-2.sslip.io".to_owned()),
			..Facts::default()
		}
	}

	#[test]
	fn a_running_stateless_app_is_redeployed() {
		let v = decide(&facts());
		assert!(v.movable(), "{v:?}");
		assert_eq!(v.method, Method::Redeploy);
		assert!(v.starts);
		assert!(v.downtime.starts_with("Next to none"), "{}", v.downtime);
		assert!(
			v.warnings
				.iter()
				.any(|w| w.contains("web.10-0-0-1.sslip.io to web.10-0-0-2.sslip.io")),
			"{:?}",
			v.warnings
		);
	}

	#[test]
	fn the_method_follows_volumes_and_reachability() {
		assert_eq!(method(true, 0), Method::Redeploy);
		assert_eq!(method(true, 2), Method::StopCopy);
		assert_eq!(method(false, 0), Method::Rebuild);
		// Volumes on an unreachable server are still a stop and copy, which is refused.
		assert_eq!(method(false, 1), Method::StopCopy);
	}

	#[test]
	fn volumes_need_a_backup_destination() {
		let mut f = facts();
		f.volumes = 1;
		let v = decide(&f);
		assert_eq!(v.method, Method::StopCopy);
		assert!(!v.movable());
		assert!(v.refusals[0].contains("backup destination"), "{v:?}");
		f.backup_target = true;
		let v = decide(&f);
		assert!(v.movable(), "{v:?}");
		assert!(
			v.downtime.starts_with("Down from the stop"),
			"{}",
			v.downtime
		);
		assert!(
			v.warnings
				.iter()
				.any(|w| w.contains("volumes stay on node-01"))
		);
	}

	#[test]
	fn volumes_on_an_unreachable_server_cannot_move() {
		let mut f = facts();
		f.volumes = 1;
		f.backup_target = true;
		f.from.as_mut().unwrap().active = false;
		let v = decide(&f);
		assert!(!v.movable());
		assert!(
			v.refusals.iter().any(|r| r.contains("does not answer")),
			"{v:?}"
		);
	}

	#[test]
	fn an_unreachable_server_means_a_rebuild() {
		let mut f = facts();
		f.from.as_mut().unwrap().active = false;
		let v = decide(&f);
		assert!(v.movable(), "{v:?}");
		assert_eq!(v.method, Method::Rebuild);
		assert!(v.starts, "it ran, so it is started on the target");
		assert!(
			v.warnings
				.iter()
				.any(|w| w.contains("builds the branch again"))
		);
	}

	#[test]
	fn the_target_must_be_ready_open_and_free_of_the_slug() {
		let mut f = facts();
		f.to = Some(End {
			name: "node-02".to_owned(),
			active: false,
			draining: true,
		});
		f.slug_taken_on_target = true;
		let v = decide(&f);
		assert_eq!(v.refusals.len(), 3, "{v:?}");
		assert!(v.refusals[0].contains("not ready"));
		assert!(v.refusals[1].contains("being drained"));
		assert!(v.refusals[2].contains("`web`"));
	}

	#[test]
	fn its_own_server_is_not_a_target() {
		let mut f = facts();
		f.to_id = Some(1);
		f.to = Some(end("node-01"));
		let v = decide(&f);
		assert_eq!(v.refusals, vec!["web already runs on node-01.".to_owned()]);
	}

	#[test]
	fn server_bound_features_are_refused_with_the_way_out() {
		let mut f = facts();
		f.tailscale = true;
		f.model_links = true;
		f.mcp = true;
		f.preview = true;
		f.user_networks = vec!["backend".to_owned(), "cache".to_owned()];
		let v = decide(&f);
		assert_eq!(v.refusals.len(), 5, "{v:?}");
		assert!(
			v.refusals
				.iter()
				.any(|r| r.contains("the networks backend and cache on node-01")),
			"{v:?}"
		);
	}

	#[test]
	fn busy_applications_wait() {
		let mut f = facts();
		f.moving = true;
		f.deploying = true;
		let v = decide(&f);
		assert_eq!(v.refusals.len(), 2, "{v:?}");
	}

	#[test]
	fn without_a_target_only_its_own_checks_apply() {
		let mut f = facts();
		f.to = None;
		f.to_id = None;
		f.slug_taken_on_target = true;
		assert!(decide(&f).movable());
	}

	#[test]
	fn a_stopped_app_moves_without_downtime() {
		let mut f = facts();
		f.running = false;
		let v = decide(&f);
		assert!(v.movable());
		assert!(!v.starts, "a stopped application stays stopped");
		assert_eq!(v.downtime, "None: nothing runs now.");
		assert!(
			v.warnings[0].contains("newest image is copied over"),
			"{v:?}"
		);
		f.has_image = false;
		let v = decide(&f);
		assert!(
			v.warnings[0].contains("next deploy builds or pulls"),
			"{v:?}"
		);
	}

	#[test]
	fn domains_and_peers_are_warnings_not_refusals() {
		let mut f = facts();
		f.domains = vec!["example.com".to_owned()];
		f.peers = vec!["database pg".to_owned()];
		let v = decide(&f);
		assert!(v.movable());
		assert!(
			v.warnings
				.iter()
				.any(|w| w.contains("Point the DNS of example.com at node-02"))
		);
		assert!(v.warnings.iter().any(|w| w.contains("database pg")));
	}

	#[test]
	fn a_drain_moves_quick_ones_first_and_keeps_the_refused_last() {
		let verdict = |volumes: usize, reachable: bool, refused: bool| {
			let mut f = facts();
			f.volumes = volumes;
			f.backup_target = true;
			f.from.as_mut().unwrap().active = reachable;
			f.tailscale = refused;
			decide(&f)
		};
		let mut items = vec![
			("zeta", Some(verdict(0, true, true))),
			("db-app", Some(verdict(1, true, false))),
			("beta", Some(verdict(0, true, false))),
			("alpha", Some(verdict(0, true, false))),
			("lost", None),
		];
		order(&mut items, |i| i.1.as_ref(), |i| i.0);
		let names: Vec<&str> = items.iter().map(|i| i.0).collect();
		assert_eq!(names, vec!["alpha", "beta", "db-app", "zeta", "lost"]);
	}
}
