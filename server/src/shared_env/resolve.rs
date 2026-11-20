//! The resolver: what an application or stack really gets. Every `DeploySpec` builder goes
//! through [`app_env`] or [`stack_inputs`], so the precedence lives in one place:
//! team < project < environment < the resource's own variables.
//!
//! A value of the resource may name a shared variable explicitly, `${{shared.KEY}}`. The
//! reference is replaced by the value the shared layers resolve for `KEY`; a name nothing
//! sets is an error, never an empty string. Shared values themselves are literal.

use std::collections::{BTreeMap, BTreeSet};

use crypto::age::SecretBox;
use sqlx::PgPool;
use thiserror::Error;

use super::{Level, SharedVar, visible};
use crate::ids::{EnvironmentId, ProjectId, TeamId};
use crate::state::AppState;

#[derive(Debug, Error)]
pub enum ResolveError {
	#[error("failed to load shared variables: {0}")]
	Db(#[from] sqlx::Error),
	#[error("failed to decrypt shared variable {0}")]
	Decrypt(String),
	#[error("{}", describe_missing(.owner, .keys))]
	Missing { owner: String, keys: Vec<String> },
}

fn describe_missing(owner: &str, keys: &[String]) -> String {
	let list = keys
		.iter()
		.map(|k| format!("${{{{shared.{k}}}}}"))
		.collect::<Vec<_>>()
		.join(", ");
	format!(
		"{owner} refers to {list}, which no shared variable sets at the team, project or environment level"
	)
}

/// A shared variable after precedence: the value that wins and where it came from.
#[derive(Debug, Clone)]
pub struct Resolved {
	pub key: String,
	pub value: String,
	#[allow(
		dead_code,
		reason = "read by tests and by the cards that show where a value comes from"
	)]
	pub level: Level,
}

/// The names of the `${{shared.NAME}}` references in `text`, in order, each once.
pub fn refs(text: &str) -> Vec<String> {
	let mut found = Vec::new();
	scan(text, |name| {
		if !found.iter().any(|f| f == name) {
			found.push(name.to_owned());
		}
		None
	});
	found
}

/// Calls `f` with each reference's name; when it returns `Some`, that text replaces the
/// reference. Returns the (possibly rewritten) text.
fn scan(text: &str, mut f: impl FnMut(&str) -> Option<String>) -> String {
	const OPEN: &str = "${{";
	let mut out = String::with_capacity(text.len());
	let mut rest = text;
	while let Some(at) = rest.find(OPEN) {
		out.push_str(&rest[..at]);
		let after = &rest[at + OPEN.len()..];
		let body = after.trim_start();
		let parsed = body.strip_prefix("shared.").and_then(|b| {
			let end = b
				.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
				.unwrap_or(b.len());
			let (name, tail) = b.split_at(end);
			let tail = tail.trim_start().strip_prefix("}}")?;
			(!name.is_empty()).then_some((name, tail))
		});
		if let Some((name, tail)) = parsed {
			match f(name) {
				Some(value) => out.push_str(&value),
				None => out.push_str(&rest[at..rest.len() - tail.len()]),
			}
			rest = tail;
		} else {
			out.push_str(OPEN);
			rest = after;
		}
	}
	out.push_str(rest);
	out
}

/// `text` with every `${{shared.KEY}}` replaced from `shared`; the names it could not
/// replace when there are any.
pub fn substitute(text: &str, shared: &BTreeMap<String, String>) -> Result<String, Vec<String>> {
	let mut missing = Vec::new();
	let out = scan(text, |name| {
		let value = shared.get(name).cloned();
		if value.is_none() && !missing.iter().any(|m| m == name) {
			missing.push(name.to_owned());
		}
		value
	});
	if missing.is_empty() {
		Ok(out)
	} else {
		Err(missing)
	}
}

/// The winning row per key of `rows` (lowest precedence first), with values decrypted.
fn merge(secrets: &SecretBox, rows: Vec<SharedVar>) -> Result<Vec<Resolved>, ResolveError> {
	let mut by_key: BTreeMap<String, Resolved> = BTreeMap::new();
	for row in rows {
		let value = secrets.decrypt(&row.value).map_err(|e| {
			tracing::error!(shared_var = row.id.0, "shared variable decrypt failed: {e}");
			ResolveError::Decrypt(row.key.clone())
		})?;
		by_key.insert(
			row.key.clone(),
			Resolved {
				level: row.level(),
				key: row.key,
				value,
			},
		);
	}
	Ok(by_key.into_values().collect())
}

/// The shared variables that apply to a resource of `environment`, after precedence,
/// ordered by key.
pub async fn shared(
	db: &PgPool,
	secrets: &SecretBox,
	team: TeamId,
	project: ProjectId,
	environment: EnvironmentId,
) -> Result<Vec<Resolved>, ResolveError> {
	merge(secrets, visible(db, team, project, environment).await?)
}

/// The names of the shared variables that apply to a resource (no decryption).
pub async fn shared_keys(
	db: &PgPool,
	team: TeamId,
	project: ProjectId,
	environment: EnvironmentId,
) -> Result<BTreeSet<String>, sqlx::Error> {
	Ok(visible(db, team, project, environment)
		.await?
		.into_iter()
		.map(|r| r.key)
		.collect())
}

async fn team_of(db: &PgPool, project: ProjectId) -> Result<TeamId, sqlx::Error> {
	sqlx::query_scalar("SELECT team_id FROM projects WHERE id = $1")
		.bind(project)
		.fetch_one(db)
		.await
}

/// The environment of an application: the shared variables it inherits, then its own
/// (decrypted, in `own`) on top. A reference in an own value is replaced; a key set in
/// both is the application's.
pub async fn app_env(
	s: &AppState,
	project: ProjectId,
	environment: EnvironmentId,
	own: Vec<(String, String)>,
) -> Result<Vec<(String, String)>, ResolveError> {
	let team = team_of(&s.db, project).await?;
	let shared = shared(&s.db, &s.secrets, team, project, environment).await?;
	let lookup: BTreeMap<String, String> = shared
		.iter()
		.map(|r| (r.key.clone(), r.value.clone()))
		.collect();

	let mut out: Vec<(String, String)> = shared
		.into_iter()
		.filter(|r| !own.iter().any(|(k, _)| *k == r.key))
		.map(|r| (r.key, r.value))
		.collect();
	for (key, value) in own {
		let value = substitute(&value, &lookup).map_err(|keys| ResolveError::Missing {
			owner: format!("env var {key}"),
			keys,
		})?;
		out.push((key, value));
	}
	Ok(out)
}

/// The variables a stack's compose file is interpolated with: the shared ones below the
/// stack's own, which win. References in the stack's values are replaced.
pub async fn stack_inputs(
	s: &AppState,
	project: ProjectId,
	environment: EnvironmentId,
	own: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, ResolveError> {
	let out = app_env(s, project, environment, own.into_iter().collect()).await?;
	Ok(out.into_iter().collect())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
		pairs
			.iter()
			.map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
			.collect()
	}

	#[test]
	fn references_are_replaced_with_or_without_spaces() {
		let shared = map(&[("KEY", "abc"), ("OTHER_1", "x")]);
		assert_eq!(
			substitute("a-${{shared.KEY}}-${{ shared.OTHER_1 }}!", &shared).unwrap(),
			"a-abc-x!"
		);
	}

	#[test]
	fn missing_references_are_listed_once() {
		let err = substitute(
			"${{shared.A}} ${{shared.B}} ${{shared.A}}",
			&BTreeMap::new(),
		)
		.unwrap_err();
		assert_eq!(err, ["A", "B"]);
	}

	#[test]
	fn other_text_is_left_alone() {
		let shared = map(&[("KEY", "abc")]);
		for text in [
			"${KEY}",
			"${{secrets.KEY}}",
			"${{shared.}}",
			"${{shared.KEY",
			"${{",
			"$${{",
			"plain",
		] {
			assert_eq!(substitute(text, &shared).unwrap(), text, "{text}");
		}
	}

	#[test]
	fn refs_lists_names() {
		assert_eq!(
			refs("${{shared.A}}${{ shared.B }}${{shared.A}}"),
			["A", "B"]
		);
	}

	#[test]
	fn a_replaced_value_is_not_scanned_again() {
		let shared = map(&[("A", "${{shared.B}}")]);
		assert_eq!(
			substitute("${{shared.A}}", &shared).unwrap(),
			"${{shared.B}}"
		);
	}
}
