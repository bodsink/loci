//! A route that cannot be told apart from another route, or that leads nowhere,
//! answers nothing.
//!
//! Measured on a real service before this: 975 route nodes, 974 of them with no
//! edge to a handler, and the 760 Go routes collapsed onto 477 names because
//! the group prefix was dropped — `/:id` appeared 116 times as the same name.
//! Both failures were silent, which is the dangerous kind: the graph looked
//! populated.

use loci_graph::{EdgeType, NodeLabel};
use loci_index::IndexOptions;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

fn serial() -> MutexGuard<'static, ()> {
    static DATA_DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    static LOCK: Mutex<()> = Mutex::new(());
    let dir = DATA_DIR.get_or_init(|| tempfile::tempdir().expect("data dir"));
    std::env::set_var("LOCI_DATA_DIR", dir.path());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create dir");
    }
    std::fs::write(path, contents).expect("write");
}

fn index(root: &Path, name: &str) {
    loci_index::index_repository(
        root,
        &IndexOptions {
            name: Some(name.to_string()),
            full: true,
            hybrid_lsp: false,
        },
    )
    .expect("index");
}

fn route_paths(project: &str) -> Vec<String> {
    let (_, store) = loci_index::open_project(project).expect("open");
    let mut paths: Vec<String> = store
        .read()
        .expect("read")
        .all_nodes()
        .expect("nodes")
        .iter()
        .filter(|n| n.label == NodeLabel::Route)
        .map(|n| n.name.clone())
        .collect();
    paths.sort();
    paths
}

/// `RoutesTo` edges as `(route path, handler name)`.
fn handled(project: &str) -> Vec<(String, String)> {
    let (_, store) = loci_index::open_project(project).expect("open");
    let reader = store.read().expect("read");
    let by_id: BTreeMap<u64, String> = reader
        .all_nodes()
        .expect("nodes")
        .iter()
        .map(|n| (n.id, n.name.clone()))
        .collect();
    let mut found: Vec<(String, String)> = reader
        .all_edges()
        .expect("edges")
        .iter()
        .filter(|e| e.edge_type == EdgeType::RoutesTo)
        .filter_map(|e| Some((by_id.get(&e.src)?.clone(), by_id.get(&e.dst)?.clone())))
        .collect();
    found.sort();
    found
}

/// Reduced from the `main.go` of the service this was measured on: a versioned
/// group, groups nested inside it, and handlers that are methods on values.
const GIN_MAIN: &str = r#"package main

func main() {
	router := gin.Default()
	router.GET("/health", healthCheck)

	v1 := router.Group("/v1")
	{
		auth := v1.Group("/auth")
		{
			auth.POST("/login", authHandler.Login)
			auth.POST("/logout", authHandler.Logout)
		}

		customers := v1.Group("/customers")
		{
			customers.GET("", customerHandler.List)
			customers.GET("/:id", customerHandler.GetByID)
		}
	}
	router.Run(":8080")
}

func healthCheck(c *gin.Context) {}
"#;

const HANDLERS: &str = r#"package handlers

type AuthHandler struct{}

func (h *AuthHandler) Login(c *gin.Context) {}

func (h *AuthHandler) Logout(c *gin.Context) {}

type CustomerHandler struct{}

func (h *CustomerHandler) List(c *gin.Context) {}

func (h *CustomerHandler) GetByID(c *gin.Context) {}
"#;

fn write_service(root: &Path) {
    write(root, "go.mod", "module example.com/svc\n\ngo 1.21\n");
    write(root, "cmd/server/main.go", GIN_MAIN);
    write(root, "internal/handlers/handlers.go", HANDLERS);
}

#[test]
fn a_grouped_route_carries_every_prefix_above_it() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write_service(dir.path());
    index(dir.path(), "routes-prefix");

    assert_eq!(
        route_paths("routes-prefix"),
        vec![
            "/health",
            "/v1/auth/login",
            "/v1/auth/logout",
            "/v1/customers",
            "/v1/customers/:id",
        ]
    );
}

/// The failure that made this worth fixing: without the prefix these two are
/// both `/:id`, indistinguishable in the graph.
#[test]
fn routes_sharing_a_last_segment_stay_distinct() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "go.mod", "module example.com/svc\n\ngo 1.21\n");
    write(
        dir.path(),
        "main.go",
        r#"package main

func main() {
	router := gin.Default()
	v1 := router.Group("/v1")
	customers := v1.Group("/customers")
	customers.GET("/:id", getCustomer)
	invoices := v1.Group("/invoices")
	invoices.GET("/:id", getInvoice)
}

func getCustomer(c *gin.Context) {}

func getInvoice(c *gin.Context) {}
"#,
    );
    index(dir.path(), "routes-distinct");

    assert_eq!(
        route_paths("routes-distinct"),
        vec!["/v1/customers/:id", "/v1/invoices/:id"]
    );
}

#[test]
fn a_method_handler_gets_an_edge_to_the_method() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write_service(dir.path());
    index(dir.path(), "routes-handlers");

    assert_eq!(
        handled("routes-handlers"),
        vec![
            ("/health".to_string(), "healthCheck".to_string()),
            ("/v1/auth/login".to_string(), "Login".to_string()),
            ("/v1/auth/logout".to_string(), "Logout".to_string()),
            ("/v1/customers".to_string(), "List".to_string()),
            ("/v1/customers/:id".to_string(), "GetByID".to_string()),
        ]
    );
}

/// A closure handler has no name to point at. The route is still real, so it
/// stays in the graph without an edge rather than being dropped or pointed at
/// something invented.
#[test]
fn an_inline_handler_leaves_the_route_without_an_edge() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "go.mod", "module example.com/svc\n\ngo 1.21\n");
    write(
        dir.path(),
        "main.go",
        r#"package main

func main() {
	router := gin.Default()
	router.GET("/ping", func(c *gin.Context) {
		c.JSON(200, "pong")
	})
}
"#,
    );
    index(dir.path(), "routes-inline");

    assert_eq!(route_paths("routes-inline"), vec!["/ping"]);
    assert!(
        handled("routes-inline").is_empty(),
        "nothing named to point at"
    );
}

/// A group opened with only middleware contributes no segment of its own but
/// must not lose the prefix it sits under.
#[test]
fn a_middleware_only_group_inherits_its_parent_prefix() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "go.mod", "module example.com/svc\n\ngo 1.21\n");
    write(
        dir.path(),
        "main.go",
        r#"package main

func main() {
	router := gin.Default()
	v1 := router.Group("/v1")
	auth := v1.Group("/auth")
	limited := auth.Group("", middleware.RateLimit())
	limited.POST("/login", doLogin)
}

func doLogin(c *gin.Context) {}
"#,
    );
    index(dir.path(), "routes-middleware");

    assert_eq!(route_paths("routes-middleware"), vec!["/v1/auth/login"]);
}

/// Express keeps working: a bare function handler and no groups.
#[test]
fn an_express_route_still_resolves_its_handler() {
    let _guard = serial();
    let dir = tempfile::tempdir().expect("tempdir");
    write(
        dir.path(),
        "server.ts",
        "const app = express();\n\
         export function createOrder(req, res) {}\n\
         app.post('/orders', createOrder);\n",
    );
    index(dir.path(), "routes-express");

    assert_eq!(
        handled("routes-express"),
        vec![("/orders".to_string(), "createOrder".to_string())]
    );
}
