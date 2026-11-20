//! The plan: what applying a file would change, found by comparing the file with the project
//! as it is. Nothing here touches the database.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;

use super::doc::{ModelDoc, ProjectDoc};
use super::objects::{Fv, Key, Kind, Obj, is_collection, is_write_only};
use crate::validate;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
	Create,
	Update,
	Delete,
}

/// One field that differs. `from` is empty for a field that is new, `to` for one that goes.
/// A secret is shown as `(secret)`, never as itself.
#[derive(Debug, Clone, Serialize)]
pub struct FieldChange {
	pub field: String,
	pub from: Option<Value>,
	pub to: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Change {
	pub action: Action,
	pub kind: Kind,
	pub environment: Option<String>,
	pub name: String,
	pub fields: Vec<FieldChange>,
	/// Applying replaces what runs: a database, stack or model is reprovisioned by the apply
	/// itself, an application when it is deployed (the apply deploys it when asked to).
	pub will_redeploy: bool,
}

/// An object, or entries of one, that exist but are not in the file. With `prune` they are
/// deleted; without it they are left alone and listed here.
#[derive(Debug, Clone, Serialize)]
pub struct Extra {
	pub kind: Kind,
	pub environment: Option<String>,
	pub name: String,
	/// Empty when the object itself is missing from the file.
	pub fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Ref {
	pub kind: Kind,
	pub environment: Option<String>,
	pub name: String,
}

/// A problem that stops an apply: it names what to fix.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Issue {
	pub path: String,
	pub message: String,
}

impl Issue {
	pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
		Self {
			path: path.into(),
			message: message.into(),
		}
	}
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
	pub create: usize,
	pub update: usize,
	pub delete: usize,
	pub unchanged: usize,
}

/// The result of comparing a file with a project.
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
	/// The slug of the project the plan is for; `None` for a team-level import, which names
	/// the projects it makes in the changes.
	pub project: Option<String>,
	pub prune: bool,
	/// What an apply would do, in the order it does it.
	pub changes: Vec<Change>,
	pub extra: Vec<Extra>,
	pub unchanged: Vec<Ref>,
	/// Anything here stops an apply.
	pub issues: Vec<Issue>,
	pub summary: Summary,
}

impl Plan {
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.changes.is_empty()
	}

	#[must_use]
	pub fn has_issues(&self) -> bool {
		!self.issues.is_empty()
	}
}

/// What a file may name that lives outside it.
#[derive(Debug, Clone, Default)]
pub struct Refs {
	pub servers: BTreeSet<String>,
	pub ssh_keys: BTreeSet<String>,
	pub registries: BTreeSet<String>,
	/// `(server name, repo, revision)` of the model downloads.
	pub model_files: BTreeSet<(String, String, String)>,
}

/// Fields that cannot change once the object exists.
fn immutable(kind: Kind, field: &str) -> bool {
	match kind {
		Kind::App => matches!(field, "server" | "source"),
		Kind::Database => matches!(field, "server" | "engine" | "version"),
		Kind::Stack => field == "server",
		Kind::Model => !is_collection(field),
		Kind::Network => matches!(field, "server" | "internal"),
		Kind::Environment => false,
	}
}

fn describe(fv: Option<&Fv>) -> Option<Value> {
	fv.map(Fv::shown)
}

fn reference(key: &Key) -> Ref {
	Ref {
		kind: key.kind,
		environment: key.environment.clone(),
		name: key.name.clone(),
	}
}

pub struct Diff {
	pub changes: Vec<Change>,
	pub extra: Vec<Extra>,
	pub unchanged: Vec<Ref>,
	pub issues: Vec<Issue>,
}

fn missing_secret(key: &Key, field: &str, name: &str) -> Issue {
	Issue::new(
		format!("{}.{field}", key.path()),
		format!("the secret \"{name}\" is not provided; add it to the secrets file"),
	)
}

fn create_change(obj: &Obj, issues: &mut Vec<Issue>) -> Change {
	let mut fields = Vec::new();
	for (name, value) in &obj.fields {
		if let Fv::Unresolved(secret) = value {
			issues.push(missing_secret(&obj.key, name, secret));
		}
		fields.push(FieldChange {
			field: name.clone(),
			from: None,
			to: Some(value.shown()),
		});
	}
	Change {
		action: Action::Create,
		kind: obj.key.kind,
		environment: obj.key.environment.clone(),
		name: obj.key.name.clone(),
		fields,
		will_redeploy: obj.key.kind.runs(),
	}
}

/// Compares what is (`current`) with what the file says (`desired`).
#[must_use]
#[allow(
	clippy::too_many_lines,
	reason = "one pass over both sides reads better than helpers that share all its state"
)]
pub fn diff(current: &[Obj], desired: &[Obj], prune: bool) -> Diff {
	let now: BTreeMap<&Key, &Obj> = current.iter().map(|o| (&o.key, o)).collect();
	let want: BTreeMap<&Key, &Obj> = desired.iter().map(|o| (&o.key, o)).collect();
	let mut out = Diff {
		changes: Vec::new(),
		extra: Vec::new(),
		unchanged: Vec::new(),
		issues: Vec::new(),
	};

	for (key, wanted) in &want {
		let Some(existing) = now.get(key) else {
			out.changes.push(create_change(wanted, &mut out.issues));
			continue;
		};
		let mut fields = Vec::new();
		let mut kept = Vec::new();
		for (name, value) in &wanted.fields {
			let stored = existing.fields.get(name);
			match stored {
				Some(stored) if value.matches(stored) => {}
				Some(stored) => {
					if immutable(key.kind, name) {
						out.issues.push(Issue::new(
							format!("{}.{name}", key.path()),
							format!(
								"cannot be changed in place (it is {}, the file says {}); \
								 rename the {} in the file to make a new one, and prune the old",
								stored.shown(),
								value.shown(),
								key.kind.as_str()
							),
						));
					}
					fields.push(FieldChange {
						field: name.clone(),
						from: describe(Some(stored)),
						to: Some(value.shown()),
					});
				}
				None => {
					if let Fv::Unresolved(secret) = value {
						out.issues.push(missing_secret(key, name, secret));
					}
					fields.push(FieldChange {
						field: name.clone(),
						from: None,
						to: Some(value.shown()),
					});
				}
			}
		}
		for (name, stored) in &existing.fields {
			if wanted.fields.contains_key(name) {
				continue;
			}
			if is_collection(name) {
				if prune {
					fields.push(FieldChange {
						field: name.clone(),
						from: describe(Some(stored)),
						to: None,
					});
				} else {
					kept.push(name.clone());
				}
			} else if is_write_only(name)
				|| key.kind == Kind::Model
				|| (key.kind == Kind::Database && name == "version")
			{
				// Stored and never read back, or left to the default (a model's engine, a database's version): the file says
				// nothing, so nothing changes.
			} else if immutable(key.kind, name) {
				out.issues.push(Issue::new(
					format!("{}.{name}", key.path()),
					"cannot be changed in place; the file leaves it out",
				));
			} else {
				fields.push(FieldChange {
					field: name.clone(),
					from: describe(Some(stored)),
					to: None,
				});
			}
		}
		if key.kind == Kind::Stack
			&& let Some(f) = fields.iter().find(|f| f.field.starts_with("file."))
		{
			out.issues.push(Issue::new(
				format!("{}.{}", key.path(), f.field),
				"the files of a stack cannot be changed from a file; redeploy its template",
			));
		}
		if !kept.is_empty() {
			out.extra.push(Extra {
				kind: key.kind,
				environment: key.environment.clone(),
				name: key.name.clone(),
				fields: kept,
			});
		}
		if fields.is_empty() {
			out.unchanged.push(reference(key));
		} else {
			out.changes.push(Change {
				action: Action::Update,
				kind: key.kind,
				environment: key.environment.clone(),
				name: key.name.clone(),
				fields,
				will_redeploy: key.kind.runs(),
			});
		}
	}

	for (key, existing) in &now {
		if want.contains_key(key) {
			continue;
		}
		if prune {
			out.changes.push(Change {
				action: Action::Delete,
				kind: key.kind,
				environment: key.environment.clone(),
				name: key.name.clone(),
				fields: existing
					.fields
					.iter()
					.map(|(f, v)| FieldChange {
						field: f.clone(),
						from: Some(v.shown()),
						to: None,
					})
					.collect(),
				will_redeploy: false,
			});
		} else {
			out.extra.push(Extra {
				kind: key.kind,
				environment: key.environment.clone(),
				name: key.name.clone(),
				fields: Vec::new(),
			});
		}
	}

	// Creates and updates go in dependency order, deletes after them in the reverse order.
	let order = |c: &Change| (c.kind, c.environment.clone(), c.name.clone());
	let (mut deletes, mut rest): (Vec<_>, Vec<_>) = std::mem::take(&mut out.changes)
		.into_iter()
		.partition(|c| c.action == Action::Delete);
	rest.sort_by_key(order);
	deletes.sort_by_key(|c| std::cmp::Reverse(order(c)));
	out.changes = rest;
	out.changes.extend(deletes);
	out
}

impl Diff {
	#[must_use]
	pub fn into_plan(self, project: Option<String>, prune: bool, mut issues: Vec<Issue>) -> Plan {
		issues.extend(self.issues);
		let mut summary = Summary {
			unchanged: self.unchanged.len(),
			..Summary::default()
		};
		for change in &self.changes {
			match change.action {
				Action::Create => summary.create += 1,
				Action::Update => summary.update += 1,
				Action::Delete => summary.delete += 1,
			}
		}
		issues.dedup();
		Plan {
			project,
			prune,
			changes: self.changes,
			extra: self.extra,
			unchanged: self.unchanged,
			issues,
			summary,
		}
	}
}

fn named(set: &BTreeSet<String>, name: &str) -> bool {
	set.contains(name)
}

fn check_model(path: &str, model: &ModelDoc, refs: &Refs, issues: &mut Vec<Issue>) {
	let key = (
		model.server.clone(),
		model.repo.clone(),
		model.revision.clone(),
	);
	if !refs.model_files.contains(&key) {
		issues.push(Issue::new(
			path,
			format!(
				"no model download of {} at {} on server \"{}\"; download it first",
				model.repo, model.revision, model.server
			),
		));
	}
}

/// What is wrong with a file before it is compared with anything: unknown names, a key that
/// is not a slug, a link to a model that is not in the file.
#[must_use]
#[allow(
	clippy::too_many_lines,
	reason = "a flat list of checks, one per field a file can get wrong"
)]
pub fn validate_doc(project: &ProjectDoc, refs: &Refs) -> Vec<Issue> {
	let mut issues = Vec::new();
	let models: BTreeSet<&str> = project
		.environments
		.values()
		.flat_map(|e| e.models.keys().map(String::as_str))
		.collect();
	let mut seen: BTreeMap<(&'static str, &str), String> = BTreeMap::new();
	for (slug, env) in &project.environments {
		let at = format!("environments.{slug}");
		if !validate::slug(slug) {
			issues.push(Issue::new(
				&at,
				"an environment is named by a slug: lowercase letters, digits and hyphens",
			));
		}
		if env.name.trim().is_empty() {
			issues.push(Issue::new(format!("{at}.name"), "the name is empty"));
		}
		for (name, app) in &env.apps {
			let at = format!("{at}.apps.{name}");
			match (&app.repo, &app.image) {
				(Some(_), None) | (None, Some(_)) => {}
				_ => issues.push(Issue::new(&at, "set exactly one of repo and image")),
			}
			if !named(&refs.servers, &app.server) {
				issues.push(Issue::new(
					format!("{at}.server"),
					format!("no server named \"{}\" in this team", app.server),
				));
			}
			if let Some(key) = &app.deploy_key
				&& !named(&refs.ssh_keys, key)
			{
				issues.push(Issue::new(
					format!("{at}.deploy_key"),
					format!("no SSH key named \"{key}\" in this team"),
				));
			}
			if let Some(registry) = &app.registry
				&& !named(&refs.registries, registry)
			{
				issues.push(Issue::new(
					format!("{at}.registry"),
					format!("no registry named \"{registry}\" in this team"),
				));
			}
			if let Some(build) = &app.build
				&& !matches!(build.as_str(), "dockerfile" | "nixpacks")
			{
				issues.push(Issue::new(
					format!("{at}.build"),
					"build is dockerfile or nixpacks",
				));
			}
			if !(1..=65535).contains(&app.port) {
				issues.push(Issue::new(format!("{at}.port"), "port is 1 to 65535"));
			}
			for link in &app.models {
				if !models.contains(link.model.as_str()) {
					issues.push(Issue::new(
						format!("{at}.models"),
						format!("no model named \"{}\" in this file", link.model),
					));
				}
			}
		}
		for (name, db) in &env.databases {
			if let Some(other) = seen.insert(("database", name), slug.clone()) {
				issues.push(Issue::new(
					format!("{at}.databases.{name}"),
					format!(
						"a project's database names are unique, and \"{name}\" is also in \"{other}\""
					),
				));
			}
			let at = format!("{at}.databases.{name}");
			if !named(&refs.servers, &db.server) {
				issues.push(Issue::new(
					format!("{at}.server"),
					format!("no server named \"{}\" in this team", db.server),
				));
			}
			if !matches!(
				db.engine.as_str(),
				"postgres" | "mysql" | "mongodb" | "redis"
			) {
				issues.push(Issue::new(
					format!("{at}.engine"),
					"engine is postgres, mysql, mongodb or redis",
				));
			}
		}
		for (name, stack) in &env.stacks {
			if let Some(other) = seen.insert(("stack", name), slug.clone()) {
				issues.push(Issue::new(
					format!("{at}.stacks.{name}"),
					format!(
						"a project's stack names are unique, and \"{name}\" is also in \"{other}\""
					),
				));
			}
			let at = format!("{at}.stacks.{name}");
			if !named(&refs.servers, &stack.server) {
				issues.push(Issue::new(
					format!("{at}.server"),
					format!("no server named \"{}\" in this team", stack.server),
				));
			}
			if let Some(registry) = &stack.registry
				&& !named(&refs.registries, registry)
			{
				issues.push(Issue::new(
					format!("{at}.registry"),
					format!("no registry named \"{registry}\" in this team"),
				));
			}
		}
		for (name, model) in &env.models {
			if let Some(other) = seen.insert(("model", name), slug.clone()) {
				issues.push(Issue::new(
					format!("{at}.models.{name}"),
					format!(
						"a project's model names are unique, and \"{name}\" is also in \"{other}\""
					),
				));
			}
			let at = format!("{at}.models.{name}");
			if !named(&refs.servers, &model.server) {
				issues.push(Issue::new(
					format!("{at}.server"),
					format!("no server named \"{}\" in this team", model.server),
				));
			} else {
				check_model(&at, model, refs, &mut issues);
			}
		}
	}
	for network in &project.networks {
		let at = format!("networks.{}", network.name);
		if !named(&refs.servers, &network.server) {
			issues.push(Issue::new(
				format!("{at}.server"),
				format!("no server named \"{}\" in this team", network.server),
			));
		}
		for member in &network.members {
			let exists =
				project
					.environments
					.get(&member.environment)
					.is_some_and(|e| match member.kind.as_str() {
						"application" => e.apps.contains_key(&member.name),
						"database" => e.databases.contains_key(&member.name),
						"stack" => e.stacks.contains_key(&member.name),
						"model" => e.models.contains_key(&member.name),
						_ => false,
					});
			if !exists {
				issues.push(Issue::new(
					format!("{at}.members"),
					format!(
						"{} \"{}\" in environment \"{}\" is not in this file",
						member.kind, member.name, member.environment
					),
				));
			}
		}
	}
	issues
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

	use serde_json::json;

	use super::*;

	fn obj(kind: Kind, name: &str, fields: &[(&str, Fv)]) -> Obj {
		Obj {
			key: Key {
				kind,
				environment: Some("production".into()),
				name: name.into(),
			},
			fields: fields
				.iter()
				.map(|(k, v)| ((*k).to_owned(), v.clone()))
				.collect::<BTreeMap<_, _>>(),
		}
	}

	fn v(x: impl Into<Value>) -> Fv {
		Fv::Val(x.into())
	}

	#[test]
	fn create_update_delete_and_unchanged_are_told_apart() {
		let current = vec![
			obj(Kind::App, "same", &[("port", v(80))]),
			obj(Kind::App, "edit", &[("port", v(80)), ("ref", v("main"))]),
			obj(Kind::App, "gone", &[("port", v(80))]),
		];
		let desired = vec![
			obj(Kind::App, "same", &[("port", v(80))]),
			obj(Kind::App, "edit", &[("port", v(81)), ("ref", v("main"))]),
			obj(Kind::App, "fresh", &[("port", v(80))]),
		];
		let off = diff(&current, &desired, false);
		assert_eq!(off.unchanged.len(), 1);
		let actions: Vec<_> = off
			.changes
			.iter()
			.map(|c| (c.action, c.name.as_str()))
			.collect();
		assert_eq!(
			actions,
			[(Action::Update, "edit"), (Action::Create, "fresh")]
		);
		assert_eq!(off.extra.len(), 1, "the missing one is listed, not deleted");
		assert_eq!(off.extra[0].name, "gone");
		let edit = &off.changes[0];
		assert_eq!(edit.fields.len(), 1);
		assert_eq!(edit.fields[0].from, Some(json!(80)));
		assert_eq!(edit.fields[0].to, Some(json!(81)));
		assert!(edit.will_redeploy);

		let on = diff(&current, &desired, true);
		assert_eq!(on.changes.last().unwrap().action, Action::Delete);
		assert!(on.extra.is_empty());
	}

	#[test]
	fn collections_are_pruned_entry_by_entry_and_scalars_are_unset() {
		let current = vec![obj(
			Kind::App,
			"web",
			&[
				("port", v(80)),
				("memory_mb", v(256)),
				("domain.a.example.com", v(true)),
				("domain.b.example.com", v(true)),
			],
		)];
		let desired = vec![obj(
			Kind::App,
			"web",
			&[("port", v(80)), ("domain.a.example.com", v(true))],
		)];
		let off = diff(&current, &desired, false);
		assert_eq!(off.changes.len(), 1, "memory_mb is unset");
		assert_eq!(off.changes[0].fields[0].field, "memory_mb");
		assert_eq!(off.extra[0].fields, ["domain.b.example.com"]);
		let on = diff(&current, &desired, true);
		assert_eq!(on.changes[0].fields.len(), 2);
		assert!(on.extra.is_empty());
	}

	#[test]
	fn secrets_never_appear_and_an_unresolved_one_leaves_a_stored_value_alone() {
		let current = vec![obj(
			Kind::App,
			"web",
			&[("env.KEY", Fv::Secret("d1".into())), ("port", v(80))],
		)];
		let same = vec![obj(
			Kind::App,
			"web",
			&[("env.KEY", Fv::Unresolved("x".into())), ("port", v(80))],
		)];
		let d = diff(&current, &same, false);
		assert!(d.changes.is_empty() && d.issues.is_empty());

		let changed = vec![obj(
			Kind::App,
			"web",
			&[("env.KEY", Fv::Secret("d2".into())), ("port", v(80))],
		)];
		let d = diff(&current, &changed, false);
		let text = serde_json::to_string(&d.changes).unwrap();
		assert!(!text.contains("d1") && !text.contains("d2"), "{text}");
		assert!(text.contains("(secret)"));

		let new_key = vec![obj(
			Kind::App,
			"web",
			&[
				("env.NEW", Fv::Unresolved("missing/one".into())),
				("port", v(80)),
			],
		)];
		let d = diff(&current, &new_key, false);
		assert_eq!(d.issues.len(), 1);
		assert!(d.issues[0].message.contains("missing/one"));
	}

	#[test]
	fn a_secret_missing_from_a_new_object_is_an_issue() {
		let d = diff(
			&[],
			&[obj(
				Kind::App,
				"web",
				&[("env.K", Fv::Unresolved("production/web/K".into()))],
			)],
			false,
		);
		assert_eq!(d.issues.len(), 1);
		assert_eq!(d.issues[0].path, "production/web.env.K");
	}

	#[test]
	fn what_cannot_change_in_place_is_an_issue() {
		let current = vec![obj(Kind::Database, "db", &[("engine", v("postgres"))])];
		let desired = vec![obj(Kind::Database, "db", &[("engine", v("mysql"))])];
		let d = diff(&current, &desired, false);
		assert_eq!(d.issues.len(), 1);
		assert!(d.issues[0].message.contains("cannot be changed in place"));
	}

	#[test]
	fn deletes_come_last_and_in_reverse_dependency_order() {
		let current = vec![
			obj(Kind::Environment, "old", &[("name", v("Old"))]),
			obj(Kind::App, "a", &[("port", v(1))]),
			obj(Kind::Database, "d", &[("engine", v("redis"))]),
		];
		let d = diff(&current, &[], true);
		let kinds: Vec<_> = d.changes.iter().map(|c| c.kind).collect();
		assert_eq!(kinds, [Kind::App, Kind::Database, Kind::Environment]);
	}
}
