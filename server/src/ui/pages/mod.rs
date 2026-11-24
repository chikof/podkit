//! The pages, one folder per area. Each area has a `router()` that `ui/mod.rs` merges, and
//! owns its templates and their tests; a card that rewrites an area replaces only its own
//! folder. Cross-cutting chrome (layout, shell, components) lives one level up.
//!
//! To add an area: create `pages/<area>/mod.rs` with `pub fn router()`, add a `pub mod`
//! line here and one `.merge(pages::<area>::router())` line in `ui/mod.rs`.

pub mod account_security;
pub mod accounts;
pub mod apps;
pub mod backups;
pub mod canvas;
pub mod cron;
pub mod databases;
pub mod firewall;
pub mod git_providers;
pub mod iac;
pub mod importers;
pub mod infrastructure;
pub mod keys;
pub mod logs;
pub mod mcp;
pub mod models;
pub mod networks;
pub mod notifications;
pub mod profile;
pub mod projects;
pub mod quadlet;
pub mod registries;
pub mod search;
pub mod security;
pub mod servers;
pub mod settings;
pub mod shared_env;
pub mod sign_in;
pub mod stacks;
pub mod templates;
pub mod terminal;
pub mod uptime;
pub mod volume_backups;
