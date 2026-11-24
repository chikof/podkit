//! Promotion: deploy the image an application runs in one environment to the matching
//! application of another environment of the same project, without building it again.
//!
//! - **The image** is the one of the source application's running deployment, exactly: the
//!   new deployment of the target carries its `image_tag` and `commit_sha`, so the Deploy job
//!   runs it the way it runs a rollback. When the two applications are on different servers,
//!   the job first moves the image from the source's server ([`super::run`], through the same
//!   relay a remote build uses).
//! - **Everything else is the target's**: its variables (and the shared ones of its
//!   environment), domains, volumes, limits, replicas and health gate. A promotion is a normal
//!   deploy of the target in every way but the source of the image.
//! - **The matching application** is found by slug, see [`find_match`]. No match, or two, is a
//!   refusal that says what was looked for.
//! - **Permission**: what deploying the target needs, `create deployment`, plus `read
//!   deployment` to look at the source's.

use sqlx::PgPool;

use super::Deployment;
use crate::applications::Application;
use crate::audit::{self, Diff, Target};
use crate::auth::permission::{Action, Resource, authorize};
use crate::environments::{self, Environment};
use crate::error::{AppResult, ServerError};
use crate::ids::{ApplicationId, DeploymentId, EnvironmentId, ProjectId, TeamId, UserId};
use crate::servers::Server;
use crate::state::AppState;
use crate::{applications, deployments, scope, servers};

/// Longest slug an application may have.
const MAX_SLUG: usize = 60;

/// Why no application of the target environment matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Miss {
	/// None has any of these slugs.
	Nothing { looked_for: Vec<String> },
	/// Several applications of the environment have this slug (on different servers).
	Several { slug: String },
}

impl Miss {
	/// The refusal, in words, for the environment called `environment`.
	#[must_use]
	pub fn message(&self, environment: &str) -> String {
		match self {
			Self::Nothing { looked_for } => format!(
				"No application in {environment} matches: looked for {}. Create it there (or clone the environment) first.",
				looked_for
					.iter()
					.map(|s| format!("`{s}`"))
					.collect::<Vec<_>>()
					.join(" or ")
			),
			Self::Several { slug } => format!(
				"More than one application in {environment} is called `{slug}`, so podkit cannot tell which one to deploy."
			),
		}
	}
}

/// The slugs that count as "the same application" in the environment `to`, best first:
///
/// 1. the same slug (`api` in staging and `api` in production, on different servers);
/// 2. the slug without the source environment's suffix (`api-staging` is `api` in production);
/// 3. that base with the target environment's suffix (`api` in production is `api-staging` in
///    staging, the name a clone or the create form gives it);
/// 4. the whole slug with the target's suffix (`api-staging` is `api-staging-qa` in qa).
///
/// Suffixes are cut the way a clone cuts them, so a long slug matches its clone.
#[must_use]
pub fn candidate_slugs(slug: &str, from_env: &str, to_env: &str) -> Vec<String> {
	let base = slug
		.strip_suffix(&format!("-{from_env}"))
		.filter(|b| !b.is_empty())
		.unwrap_or(slug);
	let mut out: Vec<String> = Vec::with_capacity(4);
	for c in [
		slug.to_owned(),
		base.to_owned(),
		environments::clone::plan::suffixed(base, to_env, MAX_SLUG),
		environments::clone::plan::suffixed(slug, to_env, MAX_SLUG),
	] {
		if !out.contains(&c) {
			out.push(c);
		}
	}
	out
}

/// The application of `items` (the target environment's) that matches the source slug: the
/// first of [`candidate_slugs`] that exactly one of them has.
///
/// # Errors
/// [`Miss::Nothing`] when none has any candidate; [`Miss::Several`] when the first candidate
/// that is present belongs to more than one.
pub fn find_match<'a, T>(
	slug: &str,
	from_env: &str,
	to_env: &str,
	items: &'a [T],
	slug_of: impl Fn(&T) -> &str,
) -> Result<&'a T, Miss> {
	let candidates = candidate_slugs(slug, from_env, to_env);
	for c in &candidates {
		let mut found = items.iter().filter(|i| slug_of(i) == c);
		match (found.next(), found.next()) {
			(Some(one), None) => return Ok(one),
			(Some(_), Some(_)) => return Err(Miss::Several { slug: c.clone() }),
			(None, _) => {}
		}
	}
	Err(Miss::Nothing {
		looked_for: candidates,
	})
}

/// The application a promotion would deploy, in one environment.
#[derive(Debug, Clone)]
pub struct Match {
	pub application: Application,
	pub server: Server,
	/// What it runs now, if anything.
	pub running: Option<Deployment>,
	/// The source runs on another server, so the image is copied over before it starts.
	pub moves_image: bool,
}

impl Match {
	/// The target already runs the very image that would be promoted.
	#[must_use]
	pub fn runs(&self, image: Option<&str>) -> bool {
		image.is_some() && self.running.as_ref().and_then(|d| d.image_tag.as_deref()) == image
	}
}

/// One other environment of the project, and what promoting to it would do.
#[derive(Debug, Clone)]
pub struct Destination {
	pub environment: Environment,
	/// The matching application, or why there is none.
	pub matched: Result<Match, String>,
}

/// What a promotion of an application would deploy, and where it could go.
#[derive(Debug, Clone)]
pub struct Plan {
	pub application: Application,
	pub environment: Environment,
	pub server: Server,
	/// The running deployment whose image would be promoted; `None` when nothing runs.
	pub source: Option<Deployment>,
	/// Every other environment of the project, in display order.
	pub destinations: Vec<Destination>,
}

impl Plan {
	/// The destination for `environment`, if it is one of the project's others.
	#[must_use]
	pub fn destination(&self, environment: EnvironmentId) -> Option<&Destination> {
		self.destinations
			.iter()
			.find(|d| d.environment.id == environment)
	}
}

/// Refusal when the source runs nothing with an image.
fn not_running(app: &Application, environment: &Environment) -> ServerError {
	ServerError::Validation(format!(
		"{} is not running in {} (no live release with an image), so there is nothing to promote. Deploy it there first.",
		app.name, environment.name
	))
}

/// The image the source application runs now: its newest `running` deployment, which must
/// have produced one.
async fn running_image(db: &PgPool, app: &Application) -> AppResult<Option<Deployment>> {
	Ok(deployments::latest_running(db, app.id)
		.await?
		.filter(|d| d.image_tag.is_some()))
}

async fn server_of(
	db: &PgPool,
	team: TeamId,
	known: &mut Vec<Server>,
	id: crate::ids::ServerId,
) -> AppResult<Server> {
	if let Some(s) = known.iter().find(|s| s.id == id) {
		return Ok(s.clone());
	}
	let s = scope::server(db, team, id).await?;
	known.push(s.clone());
	Ok(s)
}

/// What promoting `app` would do, for every other environment of its project; needs `read
/// deployment` and `read application`.
///
/// # Errors
/// The application is not the team's or the project's (404), a permission is missing, or the
/// database fails. An application that runs nothing is not an error: `source` is `None`.
pub async fn plan(
	db: &PgPool,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
) -> AppResult<Plan> {
	authorize(db, user, team, Action::Read, Resource::Application).await?;
	authorize(db, user, team, Action::Read, Resource::Deployment).await?;
	let application = scope::application(db, team, project, app).await?;
	let all = environments::list_by_project(db, application.project_id).await?;
	let environment = all
		.iter()
		.find(|e| e.id == application.environment_id)
		.cloned()
		.ok_or(ServerError::EnvironmentNotFound)?;
	let mut known = servers::list_by_team(db, team).await?;
	let server = server_of(db, team, &mut known, application.server_id).await?;
	let source = running_image(db, &application).await?;

	let mut destinations = Vec::with_capacity(all.len().saturating_sub(1));
	for env in all.into_iter().filter(|e| e.id != environment.id) {
		let apps = applications::list_by_environment(db, env.id).await?;
		let matched = match find_match(
			&application.slug,
			&environment.slug,
			&env.slug,
			&apps,
			|a| a.slug.as_str(),
		) {
			Ok(target) => {
				let target_server = server_of(db, team, &mut known, target.server_id).await?;
				Ok(Match {
					moves_image: target.server_id != application.server_id,
					running: deployments::latest_running(db, target.id).await?,
					application: target.clone(),
					server: target_server,
				})
			}
			Err(miss) => Err(miss.message(&env.name)),
		};
		destinations.push(Destination {
			environment: env,
			matched,
		});
	}
	Ok(Plan {
		application,
		environment,
		server,
		source,
		destinations,
	})
}

/// What [`promote`] started.
#[derive(Debug, Clone)]
pub struct Promoted {
	/// The new deployment, of the target application.
	pub deployment: Deployment,
	pub application: Application,
}

/// Deploys the image `app` runs now to the matching application of `to`; needs `create
/// deployment` (what deploying the target needs) and `read deployment`.
///
/// `expected` is the source deployment the caller looked at (the dialog's): when the source
/// has moved on since, the promotion is refused instead of shipping something nobody saw.
///
/// # Errors
/// 404 for an application or environment outside the team's project; a validation error when
/// `to` is the application's own environment, the source runs nothing, or nothing in `to`
/// matches; a conflict when `expected` is no longer what runs; quota and database errors.
pub async fn promote(
	s: &AppState,
	user: UserId,
	team: TeamId,
	project: ProjectId,
	app: ApplicationId,
	to: EnvironmentId,
	expected: Option<DeploymentId>,
) -> AppResult<Promoted> {
	authorize(&s.db, user, team, Action::Create, Resource::Deployment).await?;
	let plan = plan(&s.db, user, team, project, app).await?;
	// Another project's (or team's) environment is a 404, like everywhere else.
	scope::environment(&s.db, team, plan.application.project_id, to).await?;
	if to == plan.environment.id {
		return Err(ServerError::Validation(format!(
			"{} is already in {}; pick another environment.",
			plan.application.name, plan.environment.name
		)));
	}
	let source = plan
		.source
		.as_ref()
		.ok_or_else(|| not_running(&plan.application, &plan.environment))?;
	if let Some(expected) = expected.filter(|e| *e != source.id) {
		return Err(ServerError::Conflict(format!(
			"{} now runs deployment #{} in {}, not #{expected}. Look again before promoting.",
			plan.application.name, source.id, plan.environment.name
		)));
	}
	let destination = plan
		.destination(to)
		.ok_or(ServerError::EnvironmentNotFound)?;
	let target = destination
		.matched
		.as_ref()
		.map_err(|why| ServerError::Validation(why.clone()))?;

	let deployment =
		deployments::queue_promotion(s, &target.application, user, source, &plan.environment.name)
			.await?;
	audit::log(
		&s.db,
		user,
		team,
		"app.promote",
		Target::new("application", target.application.id)
			.named(&target.application.slug)
			.in_project(target.application.project_id),
		Diff::new()
			.set("deployment_id", deployment.id.0)
			.set("from_environment", plan.environment.name.as_str())
			.set("from_application", plan.application.slug.as_str())
			.set("from_deployment", source.id.0)
			.set("image", source.image_tag.as_deref().unwrap_or_default())
			.set("to_environment", destination.environment.name.as_str()),
	)
	.await;
	Ok(Promoted {
		deployment,
		application: target.application.clone(),
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	fn pick<'a>(slug: &str, from: &str, to: &str, items: &[&'a str]) -> Result<&'a str, Miss> {
		find_match(slug, from, to, items, |s: &&str| *s).copied()
	}

	#[test]
	fn the_same_slug_wins() {
		let items = ["web", "api", "api-production"];
		assert_eq!(pick("api", "staging", "production", &items), Ok("api"));
	}

	#[test]
	fn the_environment_suffix_is_dropped_and_added() {
		// staging's `api-staging` is production's `api`.
		let prod = ["web", "api"];
		assert_eq!(
			pick("api-staging", "staging", "production", &prod),
			Ok("api")
		);
		// production's `api` is staging's `api-staging`, the name a clone gives it.
		let staging = ["web-staging", "api-staging"];
		assert_eq!(
			pick("api", "production", "staging", &staging),
			Ok("api-staging")
		);
		// staging's `api-staging` is qa's `api-qa` ...
		let qa = ["api-qa"];
		assert_eq!(pick("api-staging", "staging", "qa", &qa), Ok("api-qa"));
		// ... or `api-staging-qa`, when qa was cloned from staging.
		let qa = ["api-staging-qa"];
		assert_eq!(
			pick("api-staging", "staging", "qa", &qa),
			Ok("api-staging-qa")
		);
	}

	#[test]
	fn nothing_matching_says_what_was_looked_for() {
		let items = ["web", "worker"];
		let miss = pick("api", "staging", "production", &items).unwrap_err();
		assert_eq!(
			miss,
			Miss::Nothing {
				looked_for: vec!["api".to_owned(), "api-production".to_owned()]
			}
		);
		let text = miss.message("Production");
		assert!(text.contains("No application in Production"), "{text}");
		assert!(text.contains("`api` or `api-production`"), "{text}");
	}

	#[test]
	fn two_with_the_same_slug_are_refused() {
		// Slugs are unique per server, so one environment can hold two `api` on two servers.
		let items = ["api", "api"];
		assert_eq!(
			pick("api", "staging", "production", &items),
			Err(Miss::Several {
				slug: "api".to_owned()
			})
		);
	}

	#[test]
	fn a_slug_that_is_only_the_suffix_is_not_emptied() {
		// `-staging` alone would leave nothing; the slug is kept whole.
		let c = candidate_slugs("staging", "staging", "production");
		assert_eq!(
			c,
			vec!["staging".to_owned(), "staging-production".to_owned()]
		);
	}

	#[test]
	fn a_long_slug_is_cut_like_a_clone_cuts_it() {
		let long = "a".repeat(60);
		let c = candidate_slugs(&long, "production", "staging");
		assert!(c.iter().all(|s| s.len() <= MAX_SLUG), "{c:?}");
		assert_eq!(
			c[1],
			environments::clone::plan::suffixed(&long, "staging", MAX_SLUG)
		);
	}
}
