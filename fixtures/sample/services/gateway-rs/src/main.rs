mod routes;
mod upstream;

use axum::routing::get;
use axum::Router;

use crate::routes::{health, proxy_orders, proxy_stock};

pub fn build_router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/orders", get(proxy_orders))
        .route("/stock", get(proxy_stock))
}

#[tokio::main]
async fn main() {
    let app = build_router();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
