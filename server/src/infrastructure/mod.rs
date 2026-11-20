//! Infrastructure: the one place that shows what runs where, and the guided setup that
//! gets a team there. It owns no resources; it reads servers, networks, keys, registries,
//! git providers, scans and projects through their own slices and adds one table, the
//! team's setup progress (dismissed, skipped parts).

pub mod service;
pub mod setup;
mod store;
