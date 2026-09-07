use crate::upstream::{build_upstream_url, Resolve, Upstreams};

pub async fn health() -> &'static str {
    "ok"
}

pub async fn proxy_orders() -> String {
    let upstreams = Upstreams::from_env();
    let base = upstreams.resolve("orders").unwrap_or_default();
    build_upstream_url(&base, "/orders")
}

pub async fn proxy_stock() -> String {
    let upstreams = Upstreams::from_env();
    let base = upstreams.resolve("inventory").unwrap_or_default();
    build_upstream_url(&base, "/stock")
}
