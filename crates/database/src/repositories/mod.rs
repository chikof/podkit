//! Postgres implementations of the repository traits defined in
//! `podkit_core::domain::*::repository`. Each module wraps a `&'static
//! PgPool` and implements the corresponding trait by delegating to the row
//! types in `crate::models`.

/// Postgres-backed [`ApplicationRepository`](podkit_core::domain::application::repository::ApplicationRepository).
pub mod application;
/// Postgres-backed authorization checks (permission lookups by role).
pub mod authorizer;
/// Postgres-backed [`CustomDomainRepository`](podkit_core::domain::custom_domain::repository::CustomDomainRepository).
pub mod custom_domain;
/// Postgres-backed [`DeploymentRepository`](podkit_core::domain::deployment::repository::DeploymentRepository).
pub mod deployment;
/// Postgres-backed [`EnvVarRepository`](podkit_core::domain::env_var::repository::EnvVarRepository).
pub mod env_var;
/// Postgres-backed [`PermissionRepository`](podkit_core::domain::permission::repository::PermissionRepository).
pub mod permission;
/// Postgres-backed [`ProjectRepository`](podkit_core::domain::project::repository::ProjectRepository).
pub mod project;
/// Postgres-backed [`RoleRepository`](podkit_core::domain::role::repository::RoleRepository).
pub mod role;
/// Postgres-backed [`ServerRepository`](podkit_core::domain::server::repository::ServerRepository).
pub mod server;
/// Postgres-backed [`TeamRepository`](podkit_core::domain::team::repository::TeamRepository).
pub mod team;
/// Postgres-backed [`TeamMemberRepository`](podkit_core::domain::team_member::repository::TeamMemberRepository).
pub mod team_member;
/// Postgres-backed [`UserRepository`](podkit_core::domain::user::repository::UserRepository).
pub mod user;
