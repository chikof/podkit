//! The pages, one folder per area. Each area has a `router()` that `ui/mod.rs` merges, and
//! owns its templates and their tests; a card that rewrites an area replaces only its own
//! folder. Cross-cutting chrome (layout, shell, components) lives one level up.
//!
//! To add an area: create `pages/<area>/mod.rs` with `pub fn router()`, add a `pub mod`
//! line here and one `.merge(pages::<area>::router())` line in `ui/mod.rs`.

pub mod accounts;
pub mod apps;
pub mod backups;
pub mod databases;
pub mod keys;
pub mod models;
pub mod networks;
pub mod projects;
pub mod registries;
pub mod search;
pub mod servers;
pub mod settings;
pub mod stacks;
pub mod templates;
