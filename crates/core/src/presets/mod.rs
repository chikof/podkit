//! Presets: a project as a file somebody else can apply.
//!
//! A preset is a `podkit.toml` ([`crate::iac`]) with one project in it. The file names a
//! server, a deploy key and a registry by the name they have in the team it came from, and
//! leaves every secret out, so as it stands it only applies where it was made. This module is
//! what turns it into something any team can apply:
//!
//! - [`strip`] takes those names out when a project is saved as a preset;
//! - [`needs`] says what whoever applies it has to choose or fill in;
//! - [`bind`] writes their choices into the file, which then goes through the ordinary plan
//!   and apply.
//!
//! Nothing here touches the database: the choices are checked by the plan, as for any file.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::iac::doc::{Doc, EnvironmentDoc, ProjectDoc};
use crate::iac::objects::Secrets;

/// What a server, key or registry reads as in a preset until somebody chooses one.
pub const TO_CHOOSE: &str = "${choose}";

/// The slug of the project a preset is applied to, inside an application's slug. An
/// application's slug is unique on its server across the whole install, so a saved preset
/// does not carry the one it had: `${project}-web` is `my-blog-web` in the project `my-blog`.
pub const PROJECT: &str = "${project}";

const SECRET: &str = "${secret:";
const GENERATE: &str = "${generate:";

/// Something of the team that an object of the preset needs named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Slot {
	/// `<environment>/<kind>/<name>`, or `network/<name>`: the key to answer under.
	pub at: String,
	/// What the file names, when it names something: a suggestion, not a choice.
	pub suggested: Option<String>,
}

/// What applying a preset asks of the person.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Needs {
	/// A server for each application, database, stack, model, network, and each scheduled
	/// task that runs in a container of its own.
	pub servers: Vec<Slot>,
	/// An SSH key of the team for each application that clones with one.
	pub deploy_keys: Vec<Slot>,
	/// A saved registry for each application or stack that pulls from one.
	pub registries: Vec<Slot>,
	/// Secrets to type in, by placeholder name.
	pub secrets: Vec<String>,
	/// Secrets podkit makes up, by placeholder name.
	pub generated: Vec<String>,
}

/// The person's answers to [`Needs`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Bindings {
	/// Server name by [`Slot::at`].
	pub servers: BTreeMap<String, String>,
	/// SSH key name by [`Slot::at`].
	pub deploy_keys: BTreeMap<String, String>,
	/// Registry name by [`Slot::at`].
	pub registries: BTreeMap<String, String>,
	/// Secret value by placeholder name.
	pub secrets: BTreeMap<String, String>,
}

fn only_project(doc: &Doc) -> Result<(&String, &ProjectDoc), String> {
	let mut projects = doc.projects.iter();
	match (projects.next(), projects.next()) {
		(Some(one), None) => Ok(one),
		_ => Err("a preset holds exactly one project".to_owned()),
	}
}

/// One named thing of an object: where it is answered, and the field itself.
struct Named<'a> {
	at: String,
	field: Field<'a>,
}

enum Field<'a> {
	Server(&'a mut String),
	DeployKey(&'a mut Option<String>),
	Registry(&'a mut Option<String>),
}

/// Every server, key and registry the project names, in a fixed order.
fn named(project: &mut ProjectDoc) -> Vec<Named<'_>> {
	let mut out = Vec::new();
	for (slug, env) in &mut project.environments {
		let EnvironmentDoc {
			apps,
			databases,
			stacks,
			models,
			cron,
			..
		} = env;
		for (name, app) in apps {
			let at = format!("{slug}/app/{name}");
			out.push(Named {
				at: at.clone(),
				field: Field::Server(&mut app.server),
			});
			out.push(Named {
				at: at.clone(),
				field: Field::DeployKey(&mut app.deploy_key),
			});
			out.push(Named {
				at,
				field: Field::Registry(&mut app.registry),
			});
		}
		for (name, database) in databases {
			out.push(Named {
				at: format!("{slug}/database/{name}"),
				field: Field::Server(&mut database.server),
			});
		}
		for (name, stack) in stacks {
			let at = format!("{slug}/stack/{name}");
			out.push(Named {
				at: at.clone(),
				field: Field::Server(&mut stack.server),
			});
			out.push(Named {
				at,
				field: Field::Registry(&mut stack.registry),
			});
		}
		for (name, model) in models {
			out.push(Named {
				at: format!("{slug}/model/{name}"),
				field: Field::Server(&mut model.server),
			});
		}
		for (name, task) in cron {
			// A task that runs inside its application's container has no server of its own.
			if let Some(server) = &mut task.server {
				out.push(Named {
					at: format!("{slug}/cron/{name}"),
					field: Field::Server(server),
				});
			}
		}
	}
	for network in &mut project.networks {
		out.push(Named {
			at: format!("network/{}", network.name),
			field: Field::Server(&mut network.server),
		});
	}
	out
}

fn suggestion(value: &str) -> Option<String> {
	(value != TO_CHOOSE && !value.is_empty()).then(|| value.to_owned())
}

/// The names after `prefix` in every string of `value`.
fn placeholders(value: &serde_json::Value, prefix: &str, into: &mut BTreeSet<String>) {
	match value {
		serde_json::Value::String(text) => {
			let mut rest = text.as_str();
			while let Some(start) = rest.find(prefix) {
				let after = &rest[start + prefix.len()..];
				let Some(end) = after.find('}') else { break };
				into.insert(after[..end].to_owned());
				rest = &after[end + 1..];
			}
		}
		serde_json::Value::Array(items) => {
			for item in items {
				placeholders(item, prefix, into);
			}
		}
		serde_json::Value::Object(map) => {
			for item in map.values() {
				placeholders(item, prefix, into);
			}
		}
		_ => {}
	}
}

fn as_value(doc: &Doc) -> Result<serde_json::Value, String> {
	serde_json::to_value(doc).map_err(|e| format!("the preset cannot be read: {e}"))
}

/// What applying `doc` asks of the person.
///
/// # Errors
/// A message when the file does not hold exactly one project.
pub fn needs(doc: &Doc) -> Result<Needs, String> {
	only_project(doc)?;
	let mut needs = Needs::default();
	let mut copy = doc.clone();
	let project = copy.projects.values_mut().next().ok_or("no project")?;
	for Named { at, field } in named(project) {
		match field {
			Field::Server(server) => needs.servers.push(Slot {
				at,
				suggested: suggestion(server),
			}),
			Field::DeployKey(Some(key)) => needs.deploy_keys.push(Slot {
				at,
				suggested: suggestion(key),
			}),
			Field::Registry(Some(registry)) => needs.registries.push(Slot {
				at,
				suggested: suggestion(registry),
			}),
			Field::DeployKey(None) | Field::Registry(None) => {}
		}
	}
	let value = as_value(doc)?;
	let (mut secrets, mut generated) = (BTreeSet::new(), BTreeSet::new());
	placeholders(&value, SECRET, &mut secrets);
	placeholders(&value, GENERATE, &mut generated);
	// A name podkit makes up is not also asked for.
	needs.secrets = secrets.difference(&generated).cloned().collect();
	needs.generated = generated.into_iter().collect();
	Ok(needs)
}

/// `doc` with the names that only mean something in the team it came from taken out: what is
/// saved when a project becomes a preset. Secrets are placeholders in any exported file
/// already.
#[must_use]
pub fn strip(mut doc: Doc) -> Doc {
	for project in doc.projects.values_mut() {
		for env in project.environments.values_mut() {
			// Both are unique across the install: a slug is made from the new project's, and a
			// hostname is for whoever applies the preset to give.
			for (name, app) in &mut env.apps {
				app.slug = format!("{PROJECT}-{name}");
				app.domains.clear();
			}
			for stack in env.stacks.values_mut() {
				stack.domains.clear();
			}
		}
		for Named { field, .. } in named(project) {
			match field {
				Field::Server(server) => TO_CHOOSE.clone_into(server),
				Field::DeployKey(Some(name)) | Field::Registry(Some(name)) => {
					TO_CHOOSE.clone_into(name);
				}
				Field::DeployKey(None) | Field::Registry(None) => {}
			}
		}
	}
	doc
}

/// Writes the person's answers into `doc`, names its applications after the project
/// `project_slug` it goes to, and makes up the secrets it asks podkit for.
///
/// The result is an ordinary project file and its secrets: the plan then says whether a
/// chosen server, key or registry exists, as for any file.
///
/// # Errors
/// Every need that has no answer, so the page can show them all at once.
pub fn bind(
	mut doc: Doc,
	answers: &Bindings,
	project_slug: &str,
	mut generate: impl FnMut() -> String,
) -> Result<(Doc, Secrets), Vec<String>> {
	let needs = needs(&doc).map_err(|e| vec![e])?;
	let mut missing = Vec::new();
	let project = doc.projects.values_mut().next().ok_or_else(Vec::new)?;
	for Named { at, field } in named(project) {
		let (what, answers, name) = match field {
			Field::Server(server) => ("a server", &answers.servers, server),
			Field::DeployKey(Some(key)) => ("an SSH key", &answers.deploy_keys, key),
			Field::Registry(Some(registry)) => ("a registry", &answers.registries, registry),
			Field::DeployKey(None) | Field::Registry(None) => continue,
		};
		match answers.get(&at).map(|a| a.trim()).filter(|a| !a.is_empty()) {
			Some(chosen) => chosen.clone_into(name),
			None => missing.push(format!("{at} needs {what}")),
		}
	}

	for env in project.environments.values_mut() {
		for app in env.apps.values_mut() {
			app.slug = app.slug.replace(PROJECT, project_slug);
		}
	}

	let mut secrets = answers.secrets.clone();
	for name in &needs.secrets {
		if secrets.get(name).is_none_or(String::is_empty) {
			missing.push(format!("the secret {name} needs a value"));
		}
	}
	for name in &needs.generated {
		secrets.insert(name.clone(), generate());
	}
	if !missing.is_empty() {
		return Err(missing);
	}

	// A generated secret is an ordinary one from here on.
	let text = serde_json::to_string(&doc).map_err(|e| vec![e.to_string()])?;
	let doc =
		serde_json::from_str(&text.replace(GENERATE, SECRET)).map_err(|e| vec![e.to_string()])?;
	Ok((doc, Secrets(secrets)))
}

pub mod graph;
mod routes;
pub mod service;
pub mod store;

pub use routes::router;

/// What is wrong with applying the preset `file` as a new project, with every server, key
/// and registry it asks for answered by one made-up name that exists and every secret given
/// a value. Nothing is read or made: it is how a catalog's preset is checked before it is
/// published, where there is no team.
///
/// It sees what the plan of a project file sees, such as two databases of one name. It does
/// not see what depends on an install: a slug that is taken, or whether a model the preset
/// runs has been downloaded to the server chosen for it.
///
/// # Errors
/// A message for a file that is not a preset.
pub fn dry_run(file: &str) -> Result<Vec<String>, String> {
	const STAND_IN: &str = "stand-in";
	let doc = crate::iac::doc::parse(file).map_err(|e| e.to_string())?;
	let needs = needs(&doc)?;
	let all = |slots: &[Slot]| {
		slots
			.iter()
			.map(|s| (s.at.clone(), STAND_IN.to_owned()))
			.collect()
	};
	let answers = Bindings {
		servers: all(&needs.servers),
		deploy_keys: all(&needs.deploy_keys),
		registries: all(&needs.registries),
		secrets: needs
			.secrets
			.iter()
			.map(|name| (name.clone(), STAND_IN.to_owned()))
			.collect(),
	};
	let (bound, secrets) =
		bind(doc, &answers, "preset", || STAND_IN.to_owned()).map_err(|m| m.join("; "))?;
	let mut lookup = crate::iac::snapshot::Lookup::default();
	lookup
		.servers
		.insert(STAND_IN.to_owned(), crate::ids::ServerId(1));
	lookup
		.ssh_keys
		.insert(STAND_IN.to_owned(), crate::ids::SshKeyId(1));
	lookup
		.registries
		.insert(STAND_IN.to_owned(), crate::ids::RegistryId(1));
	let (slug, project) = bound.projects.iter().next().ok_or("no project")?;
	// A model runs from a download its server already has: one a real team would have to
	// make first. Here each stands as made, so the rest of the preset is still planned.
	for model in project
		.environments
		.values()
		.flat_map(|e| e.models.values())
	{
		lookup.model_files.insert(
			(
				model.server.clone(),
				model.repo.clone(),
				model.revision.clone(),
			),
			crate::ids::ModelFileId(1),
		);
	}
	let plan = crate::iac::service::plan_of_new(slug, project, &lookup, secrets);
	Ok(plan
		.issues
		.iter()
		.map(|i| format!("{}: {}", i.path, i.message))
		.collect())
}

#[cfg(test)]
mod api_tests;
#[cfg(test)]
mod tests;
