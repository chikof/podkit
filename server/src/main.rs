use tokio::net::TcpListener;
use tracing::info;

use crate::{error::ServerError, routes::routes};

mod error;
mod routes;

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    better_tracing::fmt().init();

	info!(version = env!("CARGO_PKG_VERSION"), "podkit starting");

	let routes = routes();
	let listener = TcpListener::bind("0.0.0.0:8080").await?;

	info!("started server on http://localhost:8080");
	axum::serve(listener, routes).await?;

	Ok(())
}
