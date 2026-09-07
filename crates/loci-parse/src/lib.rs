//! Tree-sitter extraction: source text in, graph facts out.
//!
//! Only grammars listed in [`registry::BUNDLED`] are linked into the binary.
//! Asking for anything else returns [`loci_core::LociError::LanguageUnsupported`]
//! rather than a silent empty result.

pub mod dialect;
pub mod extract;
pub mod registry;
pub mod spec;

pub use dialect::{
    flatten_conditionals, neutralise_jsx_ampersands, prepare_cpp, separate_keyword_members,
};

pub use extract::{
    extract, module_prefix, CallSite, Definition, ExtractedFile, ImportRef, RouteDef, TypeRel,
    TypeRelKind,
};
pub use registry::{bundled_language_ids, grammar, is_bundled, BundledGrammar, BUNDLED};

#[cfg(test)]
mod tests {
    use super::*;
    use loci_core::LanguageId;
    use loci_graph::NodeLabel;

    fn names(extracted: &ExtractedFile, label: NodeLabel) -> Vec<String> {
        let mut found: Vec<String> = extracted
            .definitions
            .iter()
            .filter(|d| d.label == label)
            .map(|d| d.name.clone())
            .collect();
        found.sort();
        found
    }

    #[test]
    fn module_prefix_is_dotted_and_extension_free() {
        assert_eq!(
            module_prefix("services/api/orders.py"),
            "services.api.orders"
        );
        assert_eq!(module_prefix("main.go"), "main");
        assert_eq!(module_prefix("pkg/__init__.py"), "pkg");
        assert_eq!(module_prefix("src/mod.rs"), "src");
    }

    #[test]
    fn python_functions_classes_and_methods() {
        let source = r#"
import os
from services import billing

class OrderService:
    def create(self, payload):
        return validate(payload)

    def cancel(self, order_id):
        return self.create(order_id)

def validate(payload):
    return True
"#;
        let out = extract(LanguageId::Python, "api/orders.py", source).unwrap();

        assert_eq!(names(&out, NodeLabel::Class), vec!["OrderService"]);
        assert_eq!(names(&out, NodeLabel::Method), vec!["cancel", "create"]);
        assert_eq!(names(&out, NodeLabel::Function), vec!["validate"]);

        let create = out.definitions.iter().find(|d| d.name == "create").unwrap();
        assert_eq!(create.qualified_name, "api.orders.OrderService.create");

        let imports: Vec<&str> = out.imports.iter().map(|i| i.target.as_str()).collect();
        assert!(imports.contains(&"os"));
        assert!(imports.contains(&"services"));

        let callees: Vec<&str> = out.calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(callees.contains(&"validate"));
        assert!(callees.contains(&"create"));
    }

    #[test]
    fn python_call_sites_attribute_to_the_enclosing_function() {
        let source = r#"
def outer():
    inner()

def inner():
    pass
"#;
        let out = extract(LanguageId::Python, "m.py", source).unwrap();
        let call = out.calls.iter().find(|c| c.callee_name == "inner").unwrap();
        let enclosing = out.enclosing_definition(call.byte).unwrap();
        assert_eq!(enclosing.name, "outer");
    }

    #[test]
    fn python_fastapi_routes_are_extracted_with_handlers() {
        let source = r#"
from fastapi import FastAPI

app = FastAPI()

@app.get("/orders")
def list_orders():
    return []

@app.post("/orders")
def create_order():
    return {}
"#;
        let out = extract(LanguageId::Python, "api/main.py", source).unwrap();
        assert_eq!(out.routes.len(), 2);

        let get = out.routes.iter().find(|r| r.method == "GET").unwrap();
        assert_eq!(get.path, "/orders");
        assert_eq!(get.handler_name.as_deref(), Some("list_orders"));
    }

    #[test]
    fn no_routes_without_framework_evidence() {
        let source = r#"
def get(self, url):
    return url
"#;
        let out = extract(LanguageId::Python, "client.py", source).unwrap();
        assert!(out.routes.is_empty());
    }

    #[test]
    fn typescript_classes_interfaces_and_arrow_functions() {
        let source = r#"
import { Router } from "express";

export interface Order { id: string }

export class OrderStore {
  save(order: Order): void {
    persist(order);
  }
}

export const persist = (order: Order): void => {
  console.log(order);
};
"#;
        let out = extract(LanguageId::TypeScript, "src/store.ts", source).unwrap();

        assert_eq!(names(&out, NodeLabel::Interface), vec!["Order"]);
        assert_eq!(names(&out, NodeLabel::Class), vec!["OrderStore"]);
        assert!(names(&out, NodeLabel::Method).contains(&"save".to_string()));
        assert!(names(&out, NodeLabel::Function).contains(&"persist".to_string()));
    }

    #[test]
    fn typescript_express_routes() {
        let source = r#"
const app = express();
app.get("/health", healthHandler);
app.post("/orders", createOrder);
"#;
        let out = extract(LanguageId::TypeScript, "src/server.ts", source).unwrap();
        assert_eq!(out.routes.len(), 2);
        let post = out.routes.iter().find(|r| r.method == "POST").unwrap();
        assert_eq!(post.path, "/orders");
        assert_eq!(post.handler_name.as_deref(), Some("createOrder"));
    }

    #[test]
    fn go_functions_methods_structs_and_routes() {
        let source = r#"
package main

import "net/http"

type Server struct {
	addr string
}

func (s *Server) Start() error {
	return nil
}

func main() {
	http.HandleFunc("/health", healthHandler)
	run()
}

func run() {}
"#;
        let out = extract(LanguageId::Go, "cmd/main.go", source).unwrap();

        assert_eq!(names(&out, NodeLabel::Struct), vec!["Server"]);
        assert_eq!(names(&out, NodeLabel::Method), vec!["Start"]);
        assert!(names(&out, NodeLabel::Function).contains(&"main".to_string()));

        assert_eq!(out.routes.len(), 1);
        assert_eq!(out.routes[0].path, "/health");
        assert_eq!(out.routes[0].handler_name.as_deref(), Some("healthHandler"));
    }

    #[test]
    fn rust_items_and_impl_methods() {
        let source = r#"
use std::collections::HashMap;

pub struct Store {
    items: HashMap<String, String>,
}

pub trait Persist {
    fn flush(&self);
}

impl Store {
    pub fn insert(&mut self, key: String) {
        self.normalise(key);
    }

    fn normalise(&self, key: String) -> String {
        key
    }
}
"#;
        let out = extract(LanguageId::Rust, "src/store.rs", source).unwrap();

        assert_eq!(names(&out, NodeLabel::Struct), vec!["Store"]);
        assert_eq!(names(&out, NodeLabel::Trait), vec!["Persist"]);
        // Functions inside an impl block are methods; the trait's is too.
        let methods = names(&out, NodeLabel::Method);
        assert!(methods.contains(&"insert".to_string()));
        assert!(methods.contains(&"normalise".to_string()));

        let callees: Vec<&str> = out.calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(callees.contains(&"normalise"));
    }

    #[test]
    fn java_classes_methods_and_inheritance() {
        let source = r#"
package com.example;

import java.util.List;

public class OrderService extends BaseService implements Auditable {
    public void create() {
        validate();
    }

    private void validate() {}
}
"#;
        let out = extract(LanguageId::Java, "src/OrderService.java", source).unwrap();

        assert_eq!(names(&out, NodeLabel::Class), vec!["OrderService"]);
        assert_eq!(names(&out, NodeLabel::Method), vec!["create", "validate"]);

        let inherits = out
            .type_relations
            .iter()
            .find(|r| r.kind == TypeRelKind::Inherits)
            .unwrap();
        assert_eq!(inherits.subtype, "OrderService");
        assert_eq!(inherits.supertype, "BaseService");

        let implements = out
            .type_relations
            .iter()
            .find(|r| r.kind == TypeRelKind::Implements)
            .unwrap();
        assert_eq!(implements.supertype, "Auditable");
    }

    #[test]
    fn c_functions_and_includes() {
        let source = r#"
#include <stdio.h>
#include "local.h"

struct Point { int x; int y; };

int add(int a, int b) {
    return a + b;
}

int main(void) {
    return add(1, 2);
}
"#;
        let out = extract(LanguageId::C, "src/main.c", source).unwrap();

        assert!(names(&out, NodeLabel::Function).contains(&"add".to_string()));
        assert_eq!(names(&out, NodeLabel::Struct), vec!["Point"]);
        let imports: Vec<&str> = out.imports.iter().map(|i| i.target.as_str()).collect();
        assert!(imports.iter().any(|i| i.contains("stdio.h")));
    }

    #[test]
    fn cpp_classes_and_namespaces() {
        let source = r#"
namespace billing {

class Invoice {
public:
    void total();
};

void Invoice::total() {
    compute();
}

}
"#;
        let out = extract(LanguageId::Cpp, "src/invoice.cpp", source).unwrap();
        assert_eq!(names(&out, NodeLabel::Class), vec!["Invoice"]);
        assert_eq!(names(&out, NodeLabel::Module), vec!["billing"]);
    }

    #[test]
    fn javascript_functions_and_classes() {
        let source = r#"
class Cart {
  add(item) {
    return normalise(item);
  }
}

function normalise(item) {
  return item;
}
"#;
        let out = extract(LanguageId::JavaScript, "src/cart.js", source).unwrap();
        assert_eq!(names(&out, NodeLabel::Class), vec!["Cart"]);
        assert_eq!(names(&out, NodeLabel::Method), vec!["add"]);
        assert_eq!(names(&out, NodeLabel::Function), vec!["normalise"]);
    }

    #[test]
    fn rust_axum_routes_take_the_verb_from_the_wrapper_call() {
        let source = r#"
pub fn build_router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/orders", post(create_order))
}
"#;
        let out = extract(LanguageId::Rust, "src/main.rs", source).unwrap();
        assert_eq!(out.routes.len(), 2);

        let health = out.routes.iter().find(|r| r.path == "/health").unwrap();
        assert_eq!(health.method, "GET");
        assert_eq!(health.handler_name.as_deref(), Some("health"));
        // The chained builder must not stretch the route over the whole chain.
        assert_eq!(health.line, 4);

        let orders = out.routes.iter().find(|r| r.path == "/orders").unwrap();
        assert_eq!(orders.method, "POST");
        assert_eq!(orders.handler_name.as_deref(), Some("create_order"));
        assert_eq!(orders.line, 5);
    }

    #[test]
    fn go_handlefunc_has_no_verb_and_says_so() {
        let source = r#"
package main

func registerRoutes() {
	http.HandleFunc("/health", healthHandler)
}
"#;
        let out = extract(LanguageId::Go, "main.go", source).unwrap();
        assert_eq!(out.routes.len(), 1);
        assert_eq!(
            out.routes[0].method, "ANY",
            "HandleFunc really does accept any method; inventing GET would be a lie"
        );
        assert_eq!(out.routes[0].handler_name.as_deref(), Some("healthHandler"));
    }

    #[test]
    fn broken_source_still_yields_symbols_and_reports_error_ranges() {
        let source = r#"
def good():
    return 1

def broken(:
    pass
"#;
        let out = extract(LanguageId::Python, "m.py", source).unwrap();
        assert!(out.definitions.iter().any(|d| d.name == "good"));
        assert!(
            !out.error_ranges.is_empty(),
            "a syntax error must be reported so coverage can be honest"
        );
    }

    /// Every language the engine names can now be parsed, so `LanguageId` no
    /// longer contains a variant that fails at extraction time. PHP has no
    /// variant at all, which is how it stays out of scope.
    #[test]
    fn every_named_language_extracts_without_an_unsupported_error() {
        let cases: &[(LanguageId, &str, &str)] = &[
            (LanguageId::Python, "a.py", "def f():\n    pass\n"),
            (LanguageId::JavaScript, "a.js", "function f() {}\n"),
            (LanguageId::Jsx, "a.jsx", "function f() {}\n"),
            (LanguageId::TypeScript, "a.ts", "function f(): void {}\n"),
            (LanguageId::Tsx, "a.tsx", "function f(): void {}\n"),
            (LanguageId::Go, "a.go", "package main\nfunc f() {}\n"),
            (LanguageId::Rust, "a.rs", "fn f() {}\n"),
            (LanguageId::C, "a.c", "int f(void) { return 0; }\n"),
            (LanguageId::Cpp, "a.cpp", "int f() { return 0; }\n"),
            (LanguageId::Java, "A.java", "class A { void f() {} }\n"),
            (LanguageId::CSharp, "A.cs", "class A { void F() {} }\n"),
            (LanguageId::Kotlin, "A.kt", "fun f() {}\n"),
            (LanguageId::Perl, "a.pl", "sub f { return 1; }\n"),
        ];

        for (language, path, source) in cases {
            let out = extract(*language, path, source)
                .unwrap_or_else(|e| panic!("{language} must extract, got {e}"));
            assert!(
                !out.definitions.is_empty(),
                "{language} produced no definitions"
            );
        }

        assert_eq!(LanguageId::from_extension("php"), None, "PHP stays absent");
    }

    #[test]
    fn csharp_types_members_calls_and_inheritance() {
        let source = r#"
using System.Collections.Generic;

namespace Shop
{
    public interface IRepo { void Save(Order o); }

    public class OrderService : BaseService
    {
        public int Total { get; set; }

        public void Save(Order o)
        {
            Validate(o);
            _repo.Persist(o);
        }

        private void Validate(Order o) { }
    }

    public enum Status { New }
}
"#;
        let out = extract(LanguageId::CSharp, "src/Orders.cs", source).unwrap();

        assert_eq!(names(&out, NodeLabel::Class), vec!["OrderService"]);
        assert_eq!(names(&out, NodeLabel::Interface), vec!["IRepo"]);
        assert_eq!(names(&out, NodeLabel::Enum), vec!["Status"]);
        assert_eq!(names(&out, NodeLabel::Module), vec!["Shop"]);
        assert!(names(&out, NodeLabel::Method).contains(&"Validate".to_string()));
        assert_eq!(names(&out, NodeLabel::Field), vec!["Total"]);

        // The namespace is an enclosing definition, so it appears in the
        // qualified name after the file-derived module prefix.
        let save = out
            .definitions
            .iter()
            .find(|d| d.name == "Save" && d.qualified_name.contains("OrderService"))
            .expect("OrderService.Save");
        assert_eq!(save.label, NodeLabel::Method);
        assert_eq!(save.qualified_name, "src.Orders.Shop.OrderService.Save");

        let interface_save = out
            .definitions
            .iter()
            .find(|d| d.name == "Save" && d.qualified_name.contains("IRepo"))
            .expect("IRepo.Save");
        assert_eq!(interface_save.qualified_name, "src.Orders.Shop.IRepo.Save");

        // A receiver must be captured so method calls are not confused with
        // same-named free functions during resolution.
        let persist = out
            .calls
            .iter()
            .find(|c| c.callee_name == "Persist")
            .expect("Persist call");
        assert_eq!(persist.receiver.as_deref(), Some("_repo"));

        let imports: Vec<&str> = out.imports.iter().map(|i| i.target.as_str()).collect();
        assert!(imports.contains(&"System.Collections.Generic"));

        assert!(out
            .type_relations
            .iter()
            .any(|r| r.subtype == "OrderService" && r.supertype == "BaseService"));
    }

    #[test]
    fn csharp_aspnet_attribute_routes_carry_the_verb_and_path() {
        let source = r#"
public class OrdersController
{
    [HttpGet("/orders")]
    public string List() { return ""; }

    [HttpPost("/orders")]
    public string Create() { return ""; }
}
"#;
        let out = extract(LanguageId::CSharp, "Controllers/Orders.cs", source).unwrap();

        let mut routes: Vec<(String, String, Option<String>)> = out
            .routes
            .iter()
            .map(|r| (r.method.clone(), r.path.clone(), r.handler_name.clone()))
            .collect();
        routes.sort();

        assert_eq!(
            routes,
            vec![
                (
                    "GET".to_string(),
                    "/orders".to_string(),
                    Some("List".into())
                ),
                (
                    "POST".to_string(),
                    "/orders".to_string(),
                    Some("Create".into())
                ),
            ]
        );
    }

    #[test]
    fn kotlin_classes_functions_calls_and_delegation() {
        let source = r#"
package shop.orders

import shop.model.Order

interface Repo {
    fun save(o: Order)
}

class OrderService(val repo: Repo) : BaseService() {
    override fun save(o: Order) {
        validate(o)
        repo.persist(o)
    }

    private fun validate(o: Order) {}
}

fun topLevel(): Int = 1
"#;
        let out = extract(LanguageId::Kotlin, "src/orders.kt", source).unwrap();

        let classes = names(&out, NodeLabel::Class);
        assert!(classes.contains(&"OrderService".to_string()));
        assert!(classes.contains(&"Repo".to_string()));

        let save = out
            .definitions
            .iter()
            .find(|d| d.name == "save" && d.qualified_name.contains("OrderService"))
            .expect("OrderService.save");
        assert_eq!(save.qualified_name, "src.orders.OrderService.save");
        assert_eq!(save.label, NodeLabel::Method);

        assert!(out
            .definitions
            .iter()
            .any(|d| d.name == "topLevel" && d.label == NodeLabel::Function));

        let persist = out
            .calls
            .iter()
            .find(|c| c.callee_name == "persist")
            .expect("persist call");
        assert_eq!(persist.receiver.as_deref(), Some("repo"));

        let imports: Vec<&str> = out.imports.iter().map(|i| i.target.as_str()).collect();
        assert!(imports.contains(&"shop.model.Order"));

        assert!(out
            .type_relations
            .iter()
            .any(|r| r.subtype == "OrderService" && r.supertype == "BaseService"));
    }

    #[test]
    fn perl_subs_calls_and_use_statements() {
        let source = r#"
package Shop::Orders;
use strict;
use Shop::Model;

sub save {
    my ($self, $order) = @_;
    validate($order);
    $self->persist($order);
}

sub validate { return 1; }
1;
"#;
        let out = extract(LanguageId::Perl, "lib/Shop/Orders.pm", source).unwrap();

        assert_eq!(names(&out, NodeLabel::Function), vec!["save", "validate"]);

        let save = out.definitions.iter().find(|d| d.name == "save").unwrap();
        assert_eq!(save.qualified_name, "lib.Shop.Orders.save");

        let callees: Vec<&str> = out.calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(callees.contains(&"validate"));
        assert!(callees.contains(&"persist"));

        let persist = out
            .calls
            .iter()
            .find(|c| c.callee_name == "persist")
            .unwrap();
        assert_eq!(persist.receiver.as_deref(), Some("$self"));

        let imports: Vec<&str> = out.imports.iter().map(|i| i.target.as_str()).collect();
        assert!(imports.contains(&"Shop::Model"));
    }

    #[test]
    fn empty_file_yields_empty_results_not_invented_ones() {
        let out = extract(LanguageId::Python, "empty.py", "").unwrap();
        assert!(out.definitions.is_empty());
        assert!(out.calls.is_empty());
        assert!(out.routes.is_empty());
        assert!(out.error_ranges.is_empty());
    }
}
