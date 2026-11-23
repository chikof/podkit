//! What a preview is made of, decided without touching the database: [`Plan::build`] turns the
//! application, its variables and the databases it uses into the list the copy follows.
//!
//! The rules, each with a test:
//! - the preview gets a fresh, empty database for every database the application's variables
//!   point at (the same engine and version, new credentials), never a dump of the real one;
//! - a variable that holds the URL of such a database is rewritten to the URL of the preview's
//!   own; one that mentions a database's host in any other way is left out, because it would
//!   reach production;
//! - every other variable is left out unless it is marked "available to previews";
//! - a pull request from a fork that a maintainer has approved gets none of the marked variables
//!   either, unless the application says its forks may have them, and no stored git or registry
//!   login;
//! - the container is capped lower than the application's own.

use std::collections::HashMap;

use crate::ids::{DatabaseId, EnvVarId};

/// The longest name or slug a resource may have.
const MAX_LEN: usize = 60;

/// A variable of the application, decrypted.
#[derive(Debug, Clone)]
pub struct VarIn {
	pub id: EnvVarId,
	pub key: String,
	pub value: String,
	/// Marked "available to previews".
	pub marked: bool,
}

/// A database the application may use.
#[derive(Debug, Clone)]
pub struct DbIn {
	pub id: DatabaseId,
	pub name: String,
	/// What an application connects with, as the variable holds it.
	pub url: String,
	/// The host part of that URL.
	pub host: String,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f32>,
}

/// Why a variable is left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
	NotMarked,
	/// It names a database of the application, and it is not that database's URL.
	ReachesProduction,
	/// The pull request is from a fork and nobody has let forks read variables.
	Fork,
}

impl Why {
	#[must_use]
	pub const fn label(self) -> &'static str {
		match self {
			Self::NotMarked => "not marked for previews",
			Self::ReachesProduction => "points at a database of the application",
			Self::Fork => "withheld from pull requests of forks",
		}
	}
}

/// What becomes of one variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fate {
	/// The value is copied as it is.
	Keep,
	/// The value has the application's database URL replaced by the preview's database's.
	Rewrite {
		template: String,
	},
	Drop(Why),
}

/// The inputs of a plan.
pub struct Input<'a> {
	pub number: i64,
	pub app_name: &'a str,
	pub app_slug: &'a str,
	pub memory_limit_mb: Option<i32>,
	pub cpu_limit: Option<f32>,
	pub vars: &'a [VarIn],
	pub dbs: &'a [DbIn],
	pub from_fork: bool,
	pub settings: &'a super::Settings,
}

/// A database the preview gets.
#[derive(Debug, Clone, PartialEq)]
pub struct DbPlan {
	pub source: DatabaseId,
	pub name: String,
	pub memory_mb: i32,
	pub cpu: f32,
}

/// What a preview is made of.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
	pub env_name: String,
	pub env_slug: String,
	pub app_name: String,
	pub app_slug: String,
	pub vars: Vec<(String, Fate)>,
	pub databases: Vec<DbPlan>,
	pub memory_mb: i32,
	pub cpu: f32,
	/// The stored git token and registry login are not copied.
	pub strip_logins: bool,
}

/// The placeholder a rewritten value holds in place of the database URL.
fn placeholder(id: DatabaseId) -> String {
	format!("{{{{db:{}}}}}", id.0)
}

/// `base` cut so that it fits `max`, never ending in a hyphen.
fn cut(base: &str, max: usize) -> String {
	let head: String = base.chars().take(max).collect();
	head.trim_end_matches(['-', ' ']).to_owned()
}

/// The slug of a preview's environment: `pr-<n>-<app>`, so two applications of a project whose
/// pull requests have the same number do not meet.
#[must_use]
pub fn env_slug(number: i64, app: &str) -> String {
	cut(&format!("pr-{number}-{app}"), MAX_LEN)
}

/// The slug (and so the default domain) of a preview's application: `pr-<n>-<app>`.
#[must_use]
pub fn app_slug(number: i64, app: &str) -> String {
	cut(&format!("pr-{number}-{app}"), MAX_LEN)
}

/// The container memory a preview gets when the application does not say: half of the
/// application's own, at least 128 MB, or 512 MB when the application is not capped.
#[must_use]
pub fn default_memory(app: Option<i32>) -> i32 {
	app.map_or(512, |m| (m / 2).max(128))
}

/// Likewise for CPU: half of the application's, at least a tenth of a core, or half a core.
#[must_use]
pub fn default_cpu(app: Option<f32>) -> f32 {
	app.map_or(0.5, |c| (c / 2.0).max(0.1))
}

impl Plan {
	#[must_use]
	pub fn build(i: &Input<'_>) -> Self {
		let no_variables = i.from_fork && !i.settings.fork_variables;

		// Databases the variables mention, in the order they first appear.
		let mut used: Vec<&DbIn> = Vec::new();
		for v in i.vars {
			for d in i.dbs {
				if v.value.contains(&d.host) && !used.iter().any(|u| u.id == d.id) {
					used.push(d);
				}
			}
		}

		let vars = i
			.vars
			.iter()
			.map(|v| {
				let mut template = v.value.clone();
				let mut rewrote = false;
				for d in &used {
					if !d.url.is_empty() && template.contains(&d.url) {
						template = template.replace(&d.url, &placeholder(d.id));
						rewrote = true;
					}
				}
				let mentions = used.iter().any(|d| template.contains(&d.host));
				let fate = if mentions {
					Fate::Drop(Why::ReachesProduction)
				} else if rewrote {
					Fate::Rewrite { template }
				} else if no_variables {
					Fate::Drop(Why::Fork)
				} else if v.marked {
					Fate::Keep
				} else {
					Fate::Drop(Why::NotMarked)
				};
				(v.key.clone(), fate)
			})
			.collect();

		let databases = used
			.iter()
			.map(|d| DbPlan {
				source: d.id,
				name: crate::environments::clone::plan::suffixed(
					&d.name,
					&env_slug(i.number, i.app_slug),
					MAX_LEN,
				),
				memory_mb: d.memory_limit_mb.map_or(256, |m| (m / 2).clamp(128, 512)),
				cpu: d.cpu_limit.map_or(0.5, |c| (c / 2.0).max(0.1)),
			})
			.collect();

		Self {
			env_name: cut(&format!("PR #{} {}", i.number, i.app_name), MAX_LEN),
			env_slug: env_slug(i.number, i.app_slug),
			app_name: cut(&format!("{} PR #{}", i.app_name, i.number), MAX_LEN),
			app_slug: app_slug(i.number, i.app_slug),
			vars,
			databases,
			memory_mb: i
				.settings
				.memory_limit_mb
				.unwrap_or_else(|| default_memory(i.memory_limit_mb)),
			cpu: i
				.settings
				.cpu_limit
				.unwrap_or_else(|| default_cpu(i.cpu_limit)),
			strip_logins: no_variables,
		}
	}

	/// The variables the preview gets, as `(key, fate)`; the dropped ones are not in it.
	pub fn kept(&self) -> impl Iterator<Item = &(String, Fate)> {
		self.vars
			.iter()
			.filter(|(_, f)| !matches!(f, Fate::Drop(_)))
	}

	/// The names of the variables left out.
	#[must_use]
	pub fn dropped(&self) -> Vec<(&str, Why)> {
		self.vars
			.iter()
			.filter_map(|(k, f)| match f {
				Fate::Drop(w) => Some((k.as_str(), *w)),
				_ => None,
			})
			.collect()
	}
}

/// A rewritten value, with each placeholder replaced by the URL of that database in the preview.
#[must_use]
pub fn fill(template: &str, urls: &HashMap<DatabaseId, String>) -> String {
	let mut out = template.to_owned();
	for (id, url) in urls {
		out = out.replace(&placeholder(*id), url);
	}
	out
}
