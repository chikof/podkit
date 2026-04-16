use axum::routing::get;
use axum::Router;

async fn health() -> &'static str {
	"ok"
}

pub fn routes() -> Router {
	Router::new().route("/health", get(health))
}
