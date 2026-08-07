//! sqlx row types for every table, one module per table. These are the
//! persistence-layer counterparts of the domain entities in `podkit_core`;
//! see each module for how the two line up.

/// Row type and queries for the `applications` table.
pub mod application;
/// Row type and queries for the `custom_domains` table.
pub mod custom_domain;
/// Row type and queries for the `deployments` table.
pub mod deployment;
/// Row type and queries for the `env_vars` table.
pub mod env_var;
/// Row type and queries for the `permissions` table.
pub mod permissions;
/// Row type and queries for the `projects` table.
pub mod project;
/// Row type and queries for the `roles` table.
pub mod roles;
/// Row type and queries for the `servers` table.
pub mod server;
/// Row type and queries for the `teams` table.
pub mod team;
/// Row type and queries for the `team_members` table.
pub mod team_members;
/// Denylist queries for revoked JWTs.
pub mod token_revocations;
/// Row type and queries for the `users` table.
pub mod user;
