//! Hardware snapshots: fetched from the agent, stored on the server row.
//!
//! A failed fetch is only logged. The sync loop's health check alone decides
//! whether a server is reachable, so an old agent without `/v1/hardware` or a
//! host where `df` is missing never flips a server to `unreachable`.

use proto::Hardware;
use sqlx::types::Json;

use crate::agents::AgentClient;
use crate::ids::ServerId;

/// Asks the agent for its hardware and stores it. Returns whether it stored one.
pub async fn refresh(db: &sqlx::PgPool, server: ServerId, client: &AgentClient) -> bool {
	let hardware = match client.hardware().await {
		Ok(hardware) => hardware,
		Err(e) => {
			tracing::warn!(server = %server, error = %e, "could not fetch hardware");
			return false;
		}
	};
	match store(db, server, &hardware).await {
		Ok(()) => true,
		Err(e) => {
			tracing::warn!(server = %server, error = %e, "could not store hardware");
			false
		}
	}
}

/// Replaces the snapshot of `server`.
///
/// # Errors
/// The database rejects the write.
pub async fn store(
	db: impl sqlx::PgExecutor<'_>,
	server: ServerId,
	hardware: &Hardware,
) -> Result<(), sqlx::Error> {
	sqlx::query!(
		"UPDATE servers SET hardware = $2, hardware_at = NOW() WHERE id = $1",
		server as ServerId,
		Json(hardware) as Json<&Hardware>
	)
	.execute(db)
	.await?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use proto::{Cdi, Cpu, Disk, Gpu, Memory};
	use sqlx::PgPool;

	use super::*;
	use crate::ids::TeamId;
	use crate::servers::{self, Server, ServerStatus};

	fn sample() -> Hardware {
		Hardware {
			cpu: Cpu {
				model: "Test CPU".to_string(),
				threads: 16,
			},
			memory: Memory {
				total_mb: 30720,
				available_mb: 14000,
			},
			disks: vec![Disk {
				path: "/data".to_string(),
				free_mb: 100,
				total_mb: 200,
			}],
			gpus: vec![Gpu {
				vendor: "nvidia".to_string(),
				name: "RTX".to_string(),
				vram_mb: 12288,
				vram_used_mb: Some(10),
			}],
			cdi: Cdi {
				available: true,
				devices: vec!["nvidia.com/gpu".to_string()],
			},
		}
	}

	async fn local_server(db: &PgPool) -> Server {
		let team = TeamId::generate();
		sqlx::query("INSERT INTO teams (id, name, slug, logo) VALUES ($1, 'hw', 'hw', '')")
			.bind(team)
			.execute(db)
			.await
			.unwrap();
		let key = crate::tests::seed_key(db, team.0, "hw key").await;
		let mut server = Server::new_remote(
			team,
			"box".to_string(),
			"10.0.0.1".to_string(),
			22,
			"root".to_string(),
			crate::ids::SshKeyId(key),
			"/run/podman.sock".to_string(),
		);
		server.status = ServerStatus::Pending;
		servers::insert(db, &server).await.unwrap();
		server
	}

	#[sqlx::test]
	async fn snapshot_is_stored_and_read_back(db: PgPool) {
		let server = local_server(&db).await;
		assert!(
			servers::find(&db, server.id)
				.await
				.unwrap()
				.unwrap()
				.hardware
				.is_none()
		);

		store(&db, server.id, &sample()).await.unwrap();
		let found = servers::find(&db, server.id).await.unwrap().unwrap();
		assert_eq!(found.hardware.unwrap().0, sample());
		assert!(found.hardware_at.is_some());
		// The snapshot does not touch connectivity.
		assert_eq!(found.status, ServerStatus::Pending);
	}
}
