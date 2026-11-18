//! Search across one team's resources, for the command palette.
//!
//! Every query is scoped to the team in its `WHERE`, and a kind of resource is only
//! searched when the caller may read it, so a hit never names something the caller could
//! not open. Matching is a case-insensitive substring on the name (and the slug, for
//! projects and applications).

pub mod service;

#[cfg(test)]
mod tests;

use crate::ids::{EnvironmentId, ProjectId};

/// What a hit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
	Project,
	Environment,
	Application,
	Database,
	Model,
	Server,
}

/// One thing that matched. `id` is the resource's own id; for an environment it is the
/// environment's, and `project` says where it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
	pub kind: Kind,
	pub id: i64,
	pub name: String,
	/// The project a resource, or an environment, belongs to. `None` for a project and a
	/// server.
	pub project: Option<(ProjectId, String)>,
	/// The environment a resource lives in.
	pub environment: Option<(EnvironmentId, String)>,
}
