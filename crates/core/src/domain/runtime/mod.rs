//! Container runtime abstraction: the trait infra adapters implement, plus the
//! value types (specs, statuses, logs) that cross that boundary.

/// The `ContainerRuntime` trait.
pub mod container_runtime;
/// Value types (specs, statuses, logs) that cross the runtime boundary.
pub mod entity;

pub use container_runtime::ContainerRuntime;
pub use entity::{
	BuildSpec, ContainerId, ContainerSpec, ContainerState, ContainerStatus, ImageRef, LogsQuery,
	PortMapping, Protocol, ResourceLimits, RestartPolicy,
};
