//! Replicas: several copies of a stateless application on one server, behind one URL.
//!
//! - [`ReplicaConfig`] is the per-application setting (`application_replicas`); without a row
//!   the application runs one container, as it always did.
//! - [`status`] turns what the agent reports about each container into the rows the header
//!   and Monitoring show, and into one verdict for the whole set.
//! - [`service`] reads and saves the setting and asks the agent for the live rows.
//!
//! The agent runs the copies (`agent::replicas`); the Traefik labels are identical on every
//! copy, so they are the servers of one Traefik service and are balanced round robin.

pub mod service;
pub mod status;

#[cfg(test)]
mod tests;

use proto::Replicas;
use sqlx::PgExecutor;

use crate::error::ServerError;
use crate::ids::ApplicationId;

/// Most replicas of one application.
pub const MAX_REPLICAS: i32 = 10;

/// The replica settings of one application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct ReplicaConfig {
	/// Containers to run, 1 to [`MAX_REPLICAS`].
	pub replicas: i32,
	/// Pin a client to one replica with a cookie.
	pub sticky: bool,
	/// New containers a rolling update may start next to the old ones.
	pub max_surge: i32,
	/// Old containers a rolling update may take away first; used when there is no surge.
	pub max_unavailable: i32,
	/// The owner vouches that the volumes can be mounted by several containers at once.
	pub shared_volumes: bool,
}

impl Default for ReplicaConfig {
	fn default() -> Self {
		Self {
			replicas: 1,
			sticky: false,
			max_surge: 1,
			max_unavailable: 0,
			shared_volumes: false,
		}
	}
}

impl ReplicaConfig {
	/// What the agent is told. Only called for an application with a saved setting; an
	/// application without one deploys as it always did. The count may be 1: the labels of a
	/// service stay the same when it is scaled down and up again.
	///
	/// Sticky sessions need a route to put the cookie on, so `routed` is whether the service
	/// has one.
	#[must_use]
	pub fn spec(&self, routed: bool) -> Replicas {
		Replicas {
			count: u32::try_from(self.replicas).unwrap_or(1),
			max_surge: u32::try_from(self.max_surge).unwrap_or(1),
			max_unavailable: u32::try_from(self.max_unavailable).unwrap_or(0),
			sticky: self.sticky && routed,
		}
	}

	/// Whether the setting runs more than one container.
	#[must_use]
	pub fn is_scaled(&self) -> bool {
		self.replicas > 1
	}

	/// Checks every field against the limits the table enforces, with a message the form can
	/// show.
	///
	/// # Errors
	/// [`ServerError::Validation`] naming the field.
	pub fn validate(&self) -> Result<(), ServerError> {
		let bad = |message: String| Err(ServerError::Validation(message));
		if !(1..=MAX_REPLICAS).contains(&self.replicas) {
			return bad(format!("Replicas must be between 1 and {MAX_REPLICAS}"));
		}
		if !(0..=MAX_REPLICAS).contains(&self.max_surge) {
			return bad(format!(
				"The surge must be between 0 and {MAX_REPLICAS} replicas"
			));
		}
		if !(0..=MAX_REPLICAS).contains(&self.max_unavailable) {
			return bad(format!(
				"The unavailable limit must be between 0 and {MAX_REPLICAS} replicas"
			));
		}
		if self.max_surge == 0 && self.max_unavailable == 0 {
			return bad(
				"A rolling update needs room to move: allow a surge or an unavailable replica"
					.to_owned(),
			);
		}
		Ok(())
	}

	/// Refuses more than one replica for an application with `volumes` unless the owner
	/// marked them shared: two containers writing one volume corrupt a database or a file
	/// store that was written for one.
	///
	/// # Errors
	/// [`ServerError::Validation`] saying what to do about it.
	pub fn check_volumes(&self, volumes: usize) -> Result<(), ServerError> {
		if self.is_scaled() && volumes > 0 && !self.shared_volumes {
			return Err(ServerError::Validation(
				"An application with volumes runs one replica, because its replicas would write \
				 the same files. Mark the volumes as safe to share, or remove them"
					.to_owned(),
			));
		}
		Ok(())
	}
}

/// The application's replica settings, if it has any.
pub async fn config(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
) -> Result<Option<ReplicaConfig>, sqlx::Error> {
	sqlx::query_as::<_, ReplicaConfig>(
		"SELECT replicas, sticky, max_surge, max_unavailable, shared_volumes \
		 FROM application_replicas WHERE application_id = $1",
	)
	.bind(app.0)
	.fetch_optional(db)
	.await
}

/// Stores the application's replica settings.
pub async fn save(
	db: impl PgExecutor<'_>,
	app: ApplicationId,
	c: &ReplicaConfig,
) -> Result<(), sqlx::Error> {
	sqlx::query(
		"INSERT INTO application_replicas \
		 (application_id, replicas, sticky, max_surge, max_unavailable, shared_volumes) \
		 VALUES ($1, $2, $3, $4, $5, $6) \
		 ON CONFLICT (application_id) DO UPDATE SET replicas = $2, sticky = $3, max_surge = $4, \
		 max_unavailable = $5, shared_volumes = $6, updated_at = NOW()",
	)
	.bind(app.0)
	.bind(c.replicas)
	.bind(c.sticky)
	.bind(c.max_surge)
	.bind(c.max_unavailable)
	.bind(c.shared_volumes)
	.execute(db)
	.await?;
	Ok(())
}
