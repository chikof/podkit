//! The layer under podkit's web UI: configuration, state, the database and its migrations,
//! every domain's service and JSON API routes, the durable job queue that drives each
//! server's `podkit-agent`, and the client that talks to it. The `server` binary starts it;
//! `podkit-ui` renders pages over it.

// This crate is a layer of one application, not an API anybody else builds on: its items are
// `pub` so that `podkit-ui` and the `server` binary can reach them, and were written (and
// documented where it matters) as the internals of a binary. The lints below only look at
// what a crate exports, so they said nothing while this code was part of `server`; allowing
// them here keeps the same lint surface as before the split.
#![allow(missing_docs)]
#![allow(
	clippy::implicit_hasher,
	clippy::missing_errors_doc,
	clippy::missing_panics_doc,
	clippy::must_use_candidate
)]

pub mod access_rules;
pub mod agents;
pub mod applications;
pub mod audit;
pub mod auth;
pub mod auto_update;
pub mod autodeploy;
pub mod avatars;
pub mod backups;
pub mod billing;
pub mod builds;
pub mod catalog;
pub mod change;
pub mod client_ip;
pub mod config;
pub mod cron;
pub mod databases;
pub mod db;
pub mod deployments;
pub mod domains;
pub mod env_vars;
pub mod environments;
pub mod error;
pub mod firewall;
pub mod git_providers;
pub mod health_gate;
pub mod iac;
pub mod ids;
pub mod importers;
pub mod infrastructure;
pub mod jobs;
pub mod links;
pub mod log_search;
pub mod logos;
pub mod logs;
pub mod maintenance;
pub mod mcp;
pub mod mcp_servers;
pub mod members;
pub mod metrics;
pub mod model_files;
pub mod models;
pub mod moves;
pub mod net;
pub mod networks;
pub mod notifications;
pub mod previews;
pub mod projects;
pub mod quadlet;
pub mod quotas;
pub mod registries;
pub mod replicas;
pub mod routes;
pub mod scanning;
pub mod scope;
pub mod search;
pub mod servers;
pub mod shared_env;
pub mod ssh_keys;
pub mod sso;
pub mod stacks;
pub mod state;
pub mod tailscale;
pub mod teams;
pub mod templates;
pub mod terminal;
pub mod throttle;
pub mod two_factor;
pub mod uptime;
pub mod validate;
pub mod volume_backups;
pub mod volumes;
pub mod webhooks;

#[cfg(test)]
mod sec_regression;
/// What the tests of this crate and of `podkit-ui` share: a test `AppState`, the router
/// over it, requests against it and the SQL the page tests may not write themselves.
#[cfg(any(test, feature = "testing"))]
pub mod tests;
