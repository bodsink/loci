/// Upstream service addresses, read from the environment at startup.
pub struct Upstreams {
    pub orders: String,
    pub inventory: String,
}

pub trait Resolve {
    fn resolve(&self, service: &str) -> Option<String>;
}

impl Upstreams {
    pub fn from_env() -> Self {
        Self {
            orders: read_var("ORDERS_BASE_URL", "http://localhost:8000"),
            inventory: read_var("INVENTORY_BASE_URL", "http://localhost:8081"),
        }
    }
}

impl Resolve for Upstreams {
    fn resolve(&self, service: &str) -> Option<String> {
        match service {
            "orders" => Some(self.orders.clone()),
            "inventory" => Some(self.inventory.clone()),
            _ => None,
        }
    }
}

fn read_var(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

pub fn build_upstream_url(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}
