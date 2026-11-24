//! Web terminal (roadmap B5): an interactive shell in a running container (an application,
//! a database or a stack service), from the browser.
//!
//! The browser opens a WebSocket to the server, the server opens one to the container's
//! agent, and the agent runs a TTY `exec` through Podman. The server is the only place
//! decisions are made: the `terminal` permission, the API-token rule (`root` only), the
//! origin guard, the idle timeout, the audit trail and the team's optional transcript.
//! The agent just wires an exec to a socket.

#[cfg(test)]
mod live_tests;
mod pump;
mod routes;
pub mod service;
#[cfg(test)]
mod tests;

pub use routes::{router, ws_router};
