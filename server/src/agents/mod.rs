//! Reaching each server's agent: a unix socket, directly for the local server
//! and through one cached `ssh -L` tunnel per remote server.

mod client;
pub mod tunnel;

pub use client::AgentClient;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tunnel::{SshTarget, SshTunnel};

use crate::error::ServerError;
use crate::ids::ServerId;
use crate::servers::Server;
use crate::state::AppState;

pub struct AgentPool {
	local_socket: PathBuf,
	// Never held across an await.
	tunnels: Mutex<HashMap<ServerId, Arc<SshTunnel>>>,
}

impl AgentPool {
	#[must_use]
	pub fn new(local_socket: PathBuf) -> Self {
		Self {
			local_socket,
			tunnels: Mutex::default(),
		}
	}

	/// A client for the server's agent, opening or reusing its tunnel.
	///
	/// # Errors
	/// `ServerNotReady` if a remote server has no pinned host key or agent
	/// socket yet; `AgentUnavailable` if the tunnel cannot be opened (the cause
	/// is logged).
	pub async fn client(
		&self,
		state: &AppState,
		server: &Server,
	) -> Result<AgentClient, ServerError> {
		if server.is_local {
			return Ok(AgentClient::new(self.local_socket.clone(), None));
		}
		let (Some(_), Some(agent_socket)) = (&server.host_key, &server.agent_socket_path) else {
			return Err(ServerError::ServerNotReady);
		};

		if let Some(tunnel) = self.live_tunnel(server.id) {
			return Ok(client_for(tunnel));
		}

		let target = SshTarget::for_server(server, &state.secrets).map_err(|e| {
			tracing::warn!(server_id = %server.id, error = %e, "cannot build ssh target");
			ServerError::AgentUnavailable
		})?;
		let opened = SshTunnel::open(&target, agent_socket).await.map_err(|e| {
			tracing::warn!(server_id = %server.id, error = %e, "ssh tunnel failed");
			ServerError::AgentUnavailable
		})?;

		let mut tunnels = self.lock();
		// A concurrent caller may have opened one while we were connecting.
		if let Some(existing) = tunnels.get(&server.id).filter(|t| t.is_alive()) {
			return Ok(client_for(Arc::clone(existing)));
		}
		let tunnel = Arc::new(opened);
		tunnels.insert(server.id, Arc::clone(&tunnel));
		Ok(client_for(tunnel))
	}

	/// Drops the cached tunnel, e.g. after a server is deleted or its agent
	/// socket changes. In-flight requests keep the old tunnel alive until they
	/// finish.
	pub fn forget(&self, id: ServerId) {
		self.lock().remove(&id);
	}

	fn live_tunnel(&self, id: ServerId) -> Option<Arc<SshTunnel>> {
		self.lock()
			.get(&id)
			.filter(|t| t.is_alive())
			.map(Arc::clone)
	}

	fn lock(&self) -> MutexGuard<'_, HashMap<ServerId, Arc<SshTunnel>>> {
		self.tunnels.lock().unwrap_or_else(PoisonError::into_inner)
	}
}

fn client_for(tunnel: Arc<SshTunnel>) -> AgentClient {
	AgentClient::new(tunnel.local_socket_path().to_path_buf(), Some(tunnel))
}
