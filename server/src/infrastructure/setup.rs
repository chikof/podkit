//! The guided setup, as pure data: which steps there are, which a person may see, and
//! whether each is done. Nothing here touches the database; `store::facts` counts what the
//! team has and [`derive`] turns the counts into steps, so the checklist can be tested from
//! fixtures and can never drift from the real state.

/// One step of the setup, in the order a person does them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
	Server,
	Proxy,
	Networks,
	Access,
	Security,
	Project,
}

impl Step {
	pub const ALL: [Self; 6] = [
		Self::Server,
		Self::Proxy,
		Self::Networks,
		Self::Access,
		Self::Security,
		Self::Project,
	];

	#[must_use]
	pub const fn slug(self) -> &'static str {
		match self {
			Self::Server => "server",
			Self::Proxy => "proxy",
			Self::Networks => "networks",
			Self::Access => "access",
			Self::Security => "security",
			Self::Project => "project",
		}
	}

	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::Server => "Server",
			Self::Proxy => "Proxy",
			Self::Networks => "Networks",
			Self::Access => "Access",
			Self::Security => "Security",
			Self::Project => "First project",
		}
	}

	#[must_use]
	pub fn from_slug(slug: &str) -> Option<Self> {
		Self::ALL.into_iter().find(|s| s.slug() == slug)
	}
}

/// A part of the setup a team may skip: shown as "Skipped" instead of "To do".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Item {
	Networks,
	SshKey,
	GitProvider,
	Registry,
}

impl Item {
	#[must_use]
	pub const fn slug(self) -> &'static str {
		match self {
			Self::Networks => "networks",
			Self::SshKey => "ssh_key",
			Self::GitProvider => "git_provider",
			Self::Registry => "registry",
		}
	}

	#[must_use]
	pub fn from_slug(slug: &str) -> Option<Self> {
		[
			Self::Networks,
			Self::SshKey,
			Self::GitProvider,
			Self::Registry,
		]
		.into_iter()
		.find(|i| i.slug() == slug)
	}

	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::Networks => "Shared networks",
			Self::SshKey => "SSH key",
			Self::GitProvider => "Git provider",
			Self::Registry => "Registry",
		}
	}
}

/// What the team has, counted. Every number is the team's own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
	pub servers: i64,
	/// Servers that are `active`: the agent answered.
	pub servers_active: i64,
	/// Servers that are `pending` or `unreachable`.
	pub servers_attention: i64,
	/// Applications whose newest deployment is running. Traefik starts with the first.
	pub running_apps: i64,
	/// Networks a person made or imported (not the per-environment ones).
	pub user_networks: i64,
	pub ssh_keys: i64,
	pub git_providers: i64,
	pub registries: i64,
	/// Servers with scanning on.
	pub scan_servers: i64,
	/// A scan has finished, so the vulnerability database has been downloaded.
	pub scan_db_ready: bool,
	pub projects: i64,
	/// Applications, databases, models and stacks.
	pub resources: i64,
}

/// What the person may read. A step about something they cannot read is not theirs to
/// see, so it is left out rather than shown locked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(
	clippy::struct_excessive_bools,
	reason = "one flag per kind of thing a role may read"
)]
pub struct Visible {
	pub server: bool,
	pub scan: bool,
	pub project: bool,
	pub ssh_key: bool,
	pub git_provider: bool,
	pub registry: bool,
}

/// What the team has stored: whether the checklist was dismissed and what was skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
	pub dismissed: bool,
	pub skipped: Vec<String>,
}

impl Progress {
	#[must_use]
	pub fn skipped(&self, item: Item) -> bool {
		self.skipped.iter().any(|s| s == item.slug())
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
	Done,
	Todo,
	Skipped,
}

impl State {
	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::Done => "Done",
			Self::Todo => "To do",
			Self::Skipped => "Skipped",
		}
	}
}

/// One line inside a step: the access step has three, the others one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
	/// The part a person may skip, if it is skippable.
	pub item: Option<Item>,
	pub label: &'static str,
	pub state: State,
	/// What is true now, one sentence.
	pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepView {
	pub step: Step,
	pub state: State,
	pub checks: Vec<Check>,
}

fn count(n: i64, one: &str, many: &str) -> String {
	if n == 1 {
		format!("1 {one}")
	} else {
		format!("{n} {many}")
	}
}

fn state_of(done: bool, skipped: bool) -> State {
	if done {
		State::Done
	} else if skipped {
		State::Skipped
	} else {
		State::Todo
	}
}

fn check(item: Option<Item>, label: &'static str, state: State, detail: String) -> Check {
	Check {
		item,
		label,
		state,
		detail,
	}
}

fn server_step(f: &Facts) -> StepView {
	let state = if f.servers_active > 0 {
		State::Done
	} else {
		State::Todo
	};
	let detail = if f.servers_active > 0 {
		format!(
			"{} answering.",
			count(f.servers_active, "server is", "servers are")
		)
	} else if f.servers_attention > 0 {
		format!(
			"{} not answering yet. Open the server to see why.",
			count(f.servers_attention, "server is", "servers are")
		)
	} else {
		"No server yet. Add one with a key from the vault.".to_owned()
	};
	StepView {
		step: Step::Server,
		state,
		checks: vec![check(None, "A server that answers", state, detail)],
	}
}

fn proxy_step(f: &Facts) -> StepView {
	let state = if f.running_apps > 0 {
		State::Done
	} else {
		State::Todo
	};
	let detail = if f.running_apps > 0 {
		"Traefik routes your running applications.".to_owned()
	} else {
		"Traefik starts on a server with its first application.".to_owned()
	};
	StepView {
		step: Step::Proxy,
		state,
		checks: vec![check(None, "Traefik", state, detail)],
	}
}

fn networks_step(f: &Facts, p: &Progress) -> StepView {
	let state = state_of(f.user_networks > 0, p.skipped(Item::Networks));
	let detail = if f.user_networks > 0 {
		format!(
			"{} made.",
			count(f.user_networks, "shared network", "shared networks")
		)
	} else if state == State::Skipped {
		"Each environment has its own private network. Shared ones can come later.".to_owned()
	} else {
		"Optional. Each environment already gets a private network.".to_owned()
	};
	StepView {
		step: Step::Networks,
		state,
		checks: vec![check(
			Some(Item::Networks),
			"Shared networks",
			state,
			detail,
		)],
	}
}

fn access_step(f: &Facts, v: Visible, p: &Progress) -> StepView {
	let mut checks = Vec::new();
	let mut line = |show: bool, item: Item, n: i64, one: &str, many: &str, empty: &str| {
		if !show {
			return;
		}
		let state = state_of(n > 0, p.skipped(item));
		let detail = if n > 0 {
			format!("{} saved.", count(n, one, many))
		} else if state == State::Skipped {
			"Skipped for now.".to_owned()
		} else {
			empty.to_owned()
		};
		checks.push(check(Some(item), item.label(), state, detail));
	};
	line(
		v.ssh_key,
		Item::SshKey,
		f.ssh_keys,
		"key",
		"keys",
		"Servers and private repositories log in with a key.",
	);
	line(
		v.git_provider,
		Item::GitProvider,
		f.git_providers,
		"provider",
		"providers",
		"Connect GitHub, GitLab or Gitea to pick repositories.",
	);
	line(
		v.registry,
		Item::Registry,
		f.registries,
		"registry",
		"registries",
		"Pull private images with a saved login.",
	);
	let all_settled = checks.iter().all(|c| c.state != State::Todo);
	let any_done = checks.iter().any(|c| c.state == State::Done);
	let state = match (all_settled, any_done) {
		(true, true) => State::Done,
		(true, false) => State::Skipped,
		(false, _) => State::Todo,
	};
	StepView {
		step: Step::Access,
		state,
		checks,
	}
}

fn security_step(f: &Facts) -> StepView {
	let on = f.scan_servers > 0;
	let state = if on && f.scan_db_ready {
		State::Done
	} else {
		State::Todo
	};
	let detail = match (on, f.scan_db_ready) {
		(false, _) => {
			"Scanning is off on every server. Turn it on in a server's settings.".to_owned()
		}
		(true, false) => {
			"Scanning is on. The vulnerability database downloads with the first scan.".to_owned()
		}
		(true, true) => "Scanning is on and the vulnerability database is downloaded.".to_owned(),
	};
	StepView {
		step: Step::Security,
		state,
		checks: vec![check(None, "Image scanning", state, detail)],
	}
}

fn project_step(f: &Facts) -> StepView {
	let state = if f.resources > 0 {
		State::Done
	} else {
		State::Todo
	};
	let detail = if f.resources > 0 {
		format!("{} made.", count(f.resources, "resource", "resources"))
	} else if f.projects > 0 {
		"The project exists. Add its first resource.".to_owned()
	} else {
		"Make a project with its production environment, then add a resource.".to_owned()
	};
	StepView {
		step: Step::Project,
		state,
		checks: vec![check(None, "A first resource", state, detail)],
	}
}

/// The steps this person sees, in order, each with its state. A step about servers is not
/// listed to someone who cannot read servers, and so on; the access step lists only the
/// kinds of access they can read.
#[must_use]
pub fn derive(facts: &Facts, visible: Visible, progress: &Progress) -> Vec<StepView> {
	let mut steps = Vec::new();
	if visible.server {
		steps.push(server_step(facts));
		steps.push(proxy_step(facts));
		steps.push(networks_step(facts, progress));
	}
	if visible.ssh_key || visible.git_provider || visible.registry {
		steps.push(access_step(facts, visible, progress));
	}
	if visible.scan {
		steps.push(security_step(facts));
	}
	if visible.project {
		steps.push(project_step(facts));
	}
	steps
}

/// Nothing left to do: every visible step is done or skipped.
#[must_use]
pub fn complete(steps: &[StepView]) -> bool {
	steps.iter().all(|s| s.state != State::Todo)
}

/// The first step still to do; the last one when none is.
#[must_use]
pub fn first_todo(steps: &[StepView]) -> Option<Step> {
	steps
		.iter()
		.find(|s| s.state == State::Todo)
		.or(steps.last())
		.map(|s| s.step)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn everything() -> Visible {
		Visible {
			server: true,
			scan: true,
			project: true,
			ssh_key: true,
			git_provider: true,
			registry: true,
		}
	}

	fn states(steps: &[StepView]) -> Vec<(Step, State)> {
		steps.iter().map(|s| (s.step, s.state)).collect()
	}

	#[test]
	fn a_team_with_nothing_has_every_step_to_do() {
		let steps = derive(&Facts::default(), everything(), &Progress::default());
		assert_eq!(steps.len(), 6);
		assert!(steps.iter().all(|s| s.state == State::Todo));
		assert!(!complete(&steps));
		assert_eq!(first_todo(&steps), Some(Step::Server));
	}

	#[test]
	fn the_local_server_alone_finishes_the_server_step_only() {
		let facts = Facts {
			servers: 1,
			servers_active: 1,
			..Facts::default()
		};
		let steps = derive(&facts, everything(), &Progress::default());
		assert_eq!(steps[0].state, State::Done);
		assert!(steps[1..].iter().all(|s| s.state == State::Todo));
		assert_eq!(first_todo(&steps), Some(Step::Proxy));
	}

	#[test]
	fn a_server_that_does_not_answer_is_still_to_do_and_says_why() {
		let facts = Facts {
			servers: 1,
			servers_attention: 1,
			..Facts::default()
		};
		let steps = derive(&facts, everything(), &Progress::default());
		assert_eq!(steps[0].state, State::Todo);
		assert!(steps[0].checks[0].detail.contains("not answering"));
	}

	#[test]
	fn each_step_reads_its_own_facts() {
		let facts = Facts {
			servers: 1,
			servers_active: 1,
			running_apps: 2,
			user_networks: 1,
			ssh_keys: 1,
			git_providers: 1,
			registries: 1,
			scan_servers: 1,
			scan_db_ready: true,
			projects: 1,
			resources: 3,
			..Facts::default()
		};
		let steps = derive(&facts, everything(), &Progress::default());
		assert!(steps.iter().all(|s| s.state == State::Done), "{steps:?}");
		assert!(complete(&steps));
	}

	#[test]
	fn scanning_needs_both_the_switch_and_the_database() {
		let on = Facts {
			scan_servers: 1,
			..Facts::default()
		};
		let steps = derive(&on, everything(), &Progress::default());
		let security = steps.iter().find(|s| s.step == Step::Security).unwrap();
		assert_eq!(security.state, State::Todo);
		assert!(
			security.checks[0]
				.detail
				.contains("downloads with the first scan")
		);
		let ready = Facts {
			scan_servers: 1,
			scan_db_ready: true,
			..Facts::default()
		};
		let steps = derive(&ready, everything(), &Progress::default());
		let security = steps.iter().find(|s| s.step == Step::Security).unwrap();
		assert_eq!(security.state, State::Done);
	}

	#[test]
	fn skipping_settles_the_optional_parts_but_never_counts_as_done() {
		let progress = Progress {
			dismissed: false,
			skipped: vec!["networks".into(), "ssh_key".into(), "git_provider".into()],
		};
		let facts = Facts {
			registries: 1,
			..Facts::default()
		};
		let steps = derive(&facts, everything(), &progress);
		let by = |s: Step| steps.iter().find(|v| v.step == s).unwrap();
		assert_eq!(by(Step::Networks).state, State::Skipped);
		// One real registry and two skips: the step is settled and done.
		assert_eq!(by(Step::Access).state, State::Done);
		let all_skipped = Progress {
			dismissed: false,
			skipped: vec!["ssh_key".into(), "git_provider".into(), "registry".into()],
		};
		let steps = derive(&Facts::default(), everything(), &all_skipped);
		let access = steps.iter().find(|v| v.step == Step::Access).unwrap();
		assert_eq!(access.state, State::Skipped);
		// A skip never turns a missing server into a done step.
		assert_eq!(steps[0].state, State::Todo);
	}

	#[test]
	fn a_skip_does_not_hide_what_exists() {
		let progress = Progress {
			dismissed: false,
			skipped: vec!["ssh_key".into()],
		};
		let facts = Facts {
			ssh_keys: 2,
			..Facts::default()
		};
		let steps = derive(&facts, everything(), &progress);
		let access = steps.iter().find(|v| v.step == Step::Access).unwrap();
		let key = access
			.checks
			.iter()
			.find(|c| c.item == Some(Item::SshKey))
			.unwrap();
		assert_eq!(key.state, State::Done);
		assert_eq!(key.detail, "2 keys saved.");
	}

	#[test]
	fn steps_the_person_cannot_read_are_left_out() {
		// A project reader sees the first-project step and nothing else.
		let only_projects = Visible {
			project: true,
			..Visible::default()
		};
		let steps = derive(&Facts::default(), only_projects, &Progress::default());
		assert_eq!(states(&steps), vec![(Step::Project, State::Todo)]);

		// Someone who reads servers and registries, but not keys or providers.
		let partial = Visible {
			server: true,
			registry: true,
			..Visible::default()
		};
		let steps = derive(&Facts::default(), partial, &Progress::default());
		assert_eq!(
			steps.iter().map(|s| s.step).collect::<Vec<_>>(),
			vec![Step::Server, Step::Proxy, Step::Networks, Step::Access]
		);
		let access = steps.iter().find(|s| s.step == Step::Access).unwrap();
		assert_eq!(access.checks.len(), 1);
		assert_eq!(access.checks[0].item, Some(Item::Registry));

		// Nobody sees a step with nothing to show.
		assert!(derive(&Facts::default(), Visible::default(), &Progress::default()).is_empty());
	}

	#[test]
	fn a_project_without_a_resource_asks_for_one() {
		let facts = Facts {
			projects: 1,
			..Facts::default()
		};
		let steps = derive(&facts, everything(), &Progress::default());
		let project = steps.iter().find(|s| s.step == Step::Project).unwrap();
		assert_eq!(project.state, State::Todo);
		assert!(project.checks[0].detail.contains("first resource"));
	}

	#[test]
	fn slugs_round_trip_and_unknown_ones_are_refused() {
		for step in Step::ALL {
			assert_eq!(Step::from_slug(step.slug()), Some(step));
		}
		for item in [
			Item::Networks,
			Item::SshKey,
			Item::GitProvider,
			Item::Registry,
		] {
			assert_eq!(Item::from_slug(item.slug()), Some(item));
		}
		assert_eq!(Step::from_slug("nope"), None);
		assert_eq!(Item::from_slug("server"), None);
	}
}
