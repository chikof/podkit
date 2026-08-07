/// Applications: deployable units backed by a git repo.
pub mod application;
/// Custom domains attached to an application.
pub mod custom_domain;
/// Deployments: individual attempts to build and run an application.
pub mod deployment;
/// Environment variables attached to an application.
pub mod env_var;
/// Permissions that make up a role.
pub mod permission;
/// Projects: the grouping of applications owned by a team.
pub mod project;
/// Roles a team member can hold.
pub mod role;
/// Container runtime abstraction and its value types.
pub mod runtime;
/// Servers: podman hosts a team can deploy containers to.
pub mod server;
/// Types shared across domain modules: ids, errors, authorization.
pub mod shared;
/// Teams: the top-level tenancy boundary.
pub mod team;
/// Team membership, linking users to teams with a role.
pub mod team_member;
/// Users and their account-level value objects.
pub mod user;
