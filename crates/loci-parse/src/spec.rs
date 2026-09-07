use loci_core::LanguageId;
use loci_graph::NodeLabel;

/// Tree-sitter queries for one language.
///
/// Capture naming is uniform across languages so the extractor stays generic:
/// `@def.<label>` marks a definition node, `@name` its identifier,
/// `@call` / `@call.name` a call site, `@import.name` an import target, and
/// `@extends` / `@implements` a supertype reference.
pub struct LanguageSpec {
    pub definitions: &'static str,
    pub references: &'static str,
    /// Empty when no framework pattern is recognised for the language. An empty
    /// query means no Route nodes, never guessed ones.
    pub routes: &'static str,
    /// Ancestor node kinds that turn a function definition into a method.
    pub method_parents: &'static [&'static str],
    /// Node kinds that introduce a naming scope (class, module, namespace).
    pub scope_kinds: &'static [&'static str],
}

/// Map a `def.*` capture suffix to a graph label.
pub fn label_for_capture(capture: &str) -> Option<NodeLabel> {
    Some(match capture.strip_prefix("def.")? {
        "function" => NodeLabel::Function,
        "method" => NodeLabel::Method,
        "class" => NodeLabel::Class,
        "interface" => NodeLabel::Interface,
        "struct" => NodeLabel::Struct,
        "enum" => NodeLabel::Enum,
        "trait" => NodeLabel::Trait,
        "type" => NodeLabel::Type,
        "module" => NodeLabel::Module,
        "field" => NodeLabel::Field,
        _ => return None,
    })
}

pub fn spec_for(language: LanguageId) -> Option<&'static LanguageSpec> {
    Some(match language {
        LanguageId::Python => &PYTHON,
        LanguageId::JavaScript | LanguageId::Jsx => &JAVASCRIPT,
        LanguageId::TypeScript | LanguageId::Tsx => &TYPESCRIPT,
        LanguageId::Go => &GO,
        LanguageId::Rust => &RUST,
        LanguageId::C => &C,
        LanguageId::Cpp => &CPP,
        LanguageId::Java => &JAVA,
        LanguageId::CSharp => &CSHARP,
        LanguageId::Kotlin => &KOTLIN,
        LanguageId::Perl => &PERL,
        LanguageId::Bash => &BASH,
        LanguageId::Toml => &TOML,
        LanguageId::Yaml => &YAML,
        LanguageId::Ini => &INI,
    })
}

static PYTHON: LanguageSpec = LanguageSpec {
    definitions: r#"
(function_definition name: (identifier) @name) @def.function
(class_definition name: (identifier) @name) @def.class
"#,
    references: r#"
(call function: (identifier) @call.name) @call
(call function: (attribute object: (_) @call.receiver attribute: (identifier) @call.name)) @call
(import_statement name: (dotted_name) @import.name) @import
(import_from_statement module_name: (dotted_name) @import.name) @import
(class_definition
  name: (identifier) @subtype
  superclasses: (argument_list (identifier) @extends))
"#,
    routes: r#"
(decorated_definition
  (decorator
    (call
      function: (attribute
        object: (identifier)
        attribute: (identifier) @route.method)
      arguments: (argument_list (string) @route.path)))
  definition: (function_definition name: (identifier) @route.handler)) @route
"#,
    method_parents: &["class_definition"],
    scope_kinds: &["class_definition", "function_definition"],
};

static JAVASCRIPT: LanguageSpec = LanguageSpec {
    definitions: r#"
(function_declaration name: (identifier) @name) @def.function
(generator_function_declaration name: (identifier) @name) @def.function
(class_declaration name: (identifier) @name) @def.class
(method_definition name: (property_identifier) @name) @def.method
(variable_declarator name: (identifier) @name value: (arrow_function)) @def.function
(variable_declarator name: (identifier) @name value: (function_expression)) @def.function
"#,
    references: r#"
(call_expression function: (identifier) @call.name) @call
(call_expression
  function: (member_expression
    object: (_) @call.receiver
    property: (property_identifier) @call.name)) @call
(import_statement source: (string) @import.name) @import
(class_declaration
  name: (identifier) @subtype
  (class_heritage (identifier) @extends))
"#,
    routes: r#"
(call_expression
  function: (member_expression
    object: (identifier)
    property: (property_identifier) @route.method)
  arguments: (arguments (string) @route.path)) @route
"#,
    method_parents: &["class_body"],
    scope_kinds: &["class_declaration", "function_declaration"],
};

static TYPESCRIPT: LanguageSpec = LanguageSpec {
    definitions: r#"
(function_declaration name: (identifier) @name) @def.function
(generator_function_declaration name: (identifier) @name) @def.function
(class_declaration name: (type_identifier) @name) @def.class
(abstract_class_declaration name: (type_identifier) @name) @def.class
(interface_declaration name: (type_identifier) @name) @def.interface
(type_alias_declaration name: (type_identifier) @name) @def.type
(enum_declaration name: (identifier) @name) @def.enum
(method_definition name: (property_identifier) @name) @def.method
(variable_declarator name: (identifier) @name value: (arrow_function)) @def.function
(variable_declarator name: (identifier) @name value: (function_expression)) @def.function
"#,
    references: r#"
(call_expression function: (identifier) @call.name) @call
(call_expression
  function: (member_expression
    object: (_) @call.receiver
    property: (property_identifier) @call.name)) @call
(import_statement source: (string) @import.name) @import
(class_declaration
  name: (type_identifier) @subtype
  (class_heritage (extends_clause value: (identifier) @extends)))
(class_declaration
  name: (type_identifier) @subtype
  (class_heritage (implements_clause (type_identifier) @implements)))
"#,
    routes: r#"
(call_expression
  function: (member_expression
    object: (identifier)
    property: (property_identifier) @route.method)
  arguments: (arguments (string) @route.path)) @route
"#,
    method_parents: &["class_body"],
    scope_kinds: &[
        "class_declaration",
        "abstract_class_declaration",
        "function_declaration",
    ],
};

static GO: LanguageSpec = LanguageSpec {
    definitions: r#"
(function_declaration name: (identifier) @name) @def.function
(method_declaration name: (field_identifier) @name) @def.method
(type_declaration (type_spec name: (type_identifier) @name type: (struct_type))) @def.struct
(type_declaration (type_spec name: (type_identifier) @name type: (interface_type))) @def.interface
"#,
    references: r#"
(call_expression function: (identifier) @call.name) @call
(call_expression
  function: (selector_expression
    operand: (_) @call.receiver
    field: (field_identifier) @call.name)) @call
(import_spec path: (interpreted_string_literal) @import.name) @import
"#,
    routes: r#"
(call_expression
  function: (selector_expression field: (field_identifier) @route.method)
  arguments: (argument_list (interpreted_string_literal) @route.path)) @route
"#,
    method_parents: &[],
    scope_kinds: &[],
};

static RUST: LanguageSpec = LanguageSpec {
    definitions: r#"
(function_item name: (identifier) @name) @def.function
(struct_item name: (type_identifier) @name) @def.struct
(enum_item name: (type_identifier) @name) @def.enum
(trait_item name: (type_identifier) @name) @def.trait
(mod_item name: (identifier) @name) @def.module
"#,
    references: r#"
(call_expression function: (identifier) @call.name) @call
(call_expression
  function: (field_expression
    value: (_) @call.receiver
    field: (field_identifier) @call.name)) @call
(call_expression function: (scoped_identifier name: (identifier) @call.name)) @call
(use_declaration argument: (scoped_identifier) @import.name) @import
(use_declaration argument: (identifier) @import.name) @import
(impl_item
  trait: (type_identifier) @implements
  type: (type_identifier) @subtype)
"#,
    routes: r#"
(call_expression
  function: (field_expression field: (field_identifier) @route.method)
  arguments: (arguments (string_literal) @route.path)) @route
"#,
    method_parents: &["impl_item", "trait_item"],
    scope_kinds: &["mod_item", "impl_item", "trait_item"],
};

static C: LanguageSpec = LanguageSpec {
    definitions: r#"
(function_definition
  declarator: (function_declarator declarator: (identifier) @name)) @def.function
(function_definition
  declarator: (pointer_declarator
    declarator: (function_declarator declarator: (identifier) @name))) @def.function
; Prototypes. A header is nothing but these, so without them a C API is
; invisible to the graph and calls into it resolve to nothing.
(declaration
  declarator: (function_declarator declarator: (identifier) @name)) @def.function
(declaration
  declarator: (pointer_declarator
    declarator: (function_declarator declarator: (identifier) @name))) @def.function
(struct_specifier name: (type_identifier) @name body: (field_declaration_list)) @def.struct
(enum_specifier name: (type_identifier) @name body: (enumerator_list)) @def.enum
"#,
    references: r#"
(call_expression function: (identifier) @call.name) @call
(preproc_include path: (string_literal) @import.name) @import
(preproc_include path: (system_lib_string) @import.name) @import
"#,
    routes: "",
    method_parents: &[],
    scope_kinds: &[],
};

static CPP: LanguageSpec = LanguageSpec {
    definitions: r#"
(function_definition
  declarator: (function_declarator declarator: (identifier) @name)) @def.function
(function_definition
  declarator: (pointer_declarator
    declarator: (function_declarator declarator: (identifier) @name))) @def.function
(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier name: (identifier) @name))) @def.method
(function_definition
  declarator: (function_declarator declarator: (field_identifier) @name)) @def.method
; Member declarations. Qt and most C++ put the whole class API in a header as
; bodiless declarations, so matching only `function_definition` loses it all.
(field_declaration
  declarator: (function_declarator declarator: (field_identifier) @name)) @def.method
(field_declaration
  declarator: (pointer_declarator
    declarator: (function_declarator declarator: (field_identifier) @name))) @def.method
(field_declaration
  declarator: (reference_declarator
    (function_declarator declarator: (field_identifier) @name))) @def.method
; Free function prototypes.
(declaration
  declarator: (function_declarator declarator: (identifier) @name)) @def.function
(declaration
  declarator: (pointer_declarator
    declarator: (function_declarator declarator: (identifier) @name))) @def.function
(class_specifier name: (type_identifier) @name body: (field_declaration_list)) @def.class
(struct_specifier name: (type_identifier) @name body: (field_declaration_list)) @def.struct
(enum_specifier name: (type_identifier) @name body: (enumerator_list)) @def.enum
(namespace_definition name: (namespace_identifier) @name) @def.module
"#,
    references: r#"
(call_expression function: (identifier) @call.name) @call
(call_expression
  function: (field_expression
    argument: (_) @call.receiver
    field: (field_identifier) @call.name)) @call
(call_expression function: (qualified_identifier name: (identifier) @call.name)) @call
(preproc_include path: (string_literal) @import.name) @import
(preproc_include path: (system_lib_string) @import.name) @import
"#,
    routes: "",
    method_parents: &["class_specifier", "struct_specifier"],
    scope_kinds: &[
        "namespace_definition",
        "class_specifier",
        "struct_specifier",
    ],
};

static JAVA: LanguageSpec = LanguageSpec {
    definitions: r#"
(class_declaration name: (identifier) @name) @def.class
(interface_declaration name: (identifier) @name) @def.interface
(enum_declaration name: (identifier) @name) @def.enum
(method_declaration name: (identifier) @name) @def.method
(constructor_declaration name: (identifier) @name) @def.method
"#,
    references: r#"
(method_invocation object: (_) @call.receiver name: (identifier) @call.name) @call
(method_invocation !object name: (identifier) @call.name) @call
(import_declaration (scoped_identifier) @import.name) @import
(class_declaration
  name: (identifier) @subtype
  (superclass (type_identifier) @extends))
(class_declaration
  name: (identifier) @subtype
  (super_interfaces (type_list (type_identifier) @implements)))
"#,
    routes: "",
    method_parents: &["class_body", "interface_body", "enum_body"],
    scope_kinds: &[
        "class_declaration",
        "interface_declaration",
        "enum_declaration",
    ],
};

static CSHARP: LanguageSpec = LanguageSpec {
    definitions: r#"
(class_declaration name: (identifier) @name) @def.class
(interface_declaration name: (identifier) @name) @def.interface
(struct_declaration name: (identifier) @name) @def.struct
(enum_declaration name: (identifier) @name) @def.enum
(record_declaration name: (identifier) @name) @def.class
(method_declaration name: (identifier) @name) @def.method
(constructor_declaration name: (identifier) @name) @def.method
(property_declaration name: (identifier) @name) @def.field
(namespace_declaration name: (identifier) @name) @def.module
(namespace_declaration name: (qualified_name) @name) @def.module
"#,
    references: r#"
(invocation_expression function: (identifier) @call.name) @call
(invocation_expression
  function: (member_access_expression
    expression: (_) @call.receiver
    name: (identifier) @call.name)) @call
(using_directive (qualified_name) @import.name) @import
(using_directive (identifier) @import.name) @import
(class_declaration
  name: (identifier) @subtype
  (base_list (identifier) @extends))
(struct_declaration
  name: (identifier) @subtype
  (base_list (identifier) @implements))
"#,
    // ASP.NET carries the verb in the attribute name (HttpGet) and the path in
    // its first string argument.
    routes: r#"
(method_declaration
  (attribute_list
    (attribute
      name: (identifier) @route.method
      (attribute_argument_list
        (attribute_argument (string_literal) @route.path))))
  name: (identifier) @route.handler) @route
"#,
    method_parents: &["declaration_list"],
    scope_kinds: &[
        "namespace_declaration",
        "class_declaration",
        "interface_declaration",
        "struct_declaration",
        "record_declaration",
        "enum_declaration",
    ],
};

static KOTLIN: LanguageSpec = LanguageSpec {
    definitions: r#"
(class_declaration name: (identifier) @name) @def.class
(object_declaration name: (identifier) @name) @def.class
(function_declaration name: (identifier) @name) @def.function
"#,
    // Kotlin's navigation_expression does not name its children, so receiver
    // and callee are matched positionally.
    references: r#"
(call_expression (identifier) @call.name) @call
(call_expression
  (navigation_expression
    (identifier) @call.receiver
    (identifier) @call.name)) @call
(import (qualified_identifier) @import.name) @import
(class_declaration
  name: (identifier) @subtype
  (delegation_specifiers
    (delegation_specifier
      (constructor_invocation (user_type (identifier) @extends)))))
(class_declaration
  name: (identifier) @subtype
  (delegation_specifiers
    (delegation_specifier (user_type (identifier) @implements))))
"#,
    routes: "",
    method_parents: &["class_body"],
    scope_kinds: &["class_declaration", "object_declaration"],
};

static PERL: LanguageSpec = LanguageSpec {
    definitions: r#"
(function_definition name: (identifier) @name) @def.function
"#,
    references: r#"
(call_expression_with_bareword function_name: (identifier) @call.name) @call
(method_invocation
  object_return_value: (_) @call.receiver
  function_name: (identifier) @call.name) @call
(use_no_statement package_name: (package_name) @import.name) @import
"#,
    routes: "",
    // A Perl package is a statement, not a container, so subs are never nested
    // inside it in the tree. Naming stays file-based rather than inventing a
    // containment relationship the AST does not express.
    method_parents: &[],
    scope_kinds: &[],
};

/// HTTP verbs recognised in framework route patterns. A call whose method name
/// is not in this set does not produce a Route node.
pub const HTTP_VERBS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "head", "options", "all",
];

/// Framework-specific registration helpers that carry the verb elsewhere.
pub const ROUTE_REGISTRARS: &[&str] = &["route", "handlefunc", "handle", "add_route"];

/// References are deliberately empty: shell calls and `source` lines are the
/// same node kind and can only be told apart by text, which this query engine
/// cannot do. `extract_shell_references` handles both by walking instead.
static BASH: LanguageSpec = LanguageSpec {
    definitions: r#"
(function_definition name: (word) @name) @def.function
"#,
    references: "",
    routes: "",
    method_parents: &[],
    scope_kinds: &[],
};

/// A table is the nearest thing TOML has to a module, and a pair to a field.
/// The table's own key is a direct child, while a pair's key sits under `pair`,
/// so the two patterns cannot collide.
static TOML: LanguageSpec = LanguageSpec {
    definitions: r#"
(table (bare_key) @name) @def.module
(table (dotted_key) @name) @def.module
(pair (bare_key) @name) @def.field
"#,
    references: "",
    routes: "",
    method_parents: &[],
    scope_kinds: &["table"],
};

/// YAML has no sections, only nesting, so a key is classified by the shape of
/// its value: a block value means it contains other keys, a scalar means it is
/// a leaf. The split is not cosmetic — qualified names are built from enclosing
/// definitions that are not fields, so without it every `runs-on` in a workflow
/// would collapse to the same name. The two patterns are mutually exclusive.
static YAML: LanguageSpec = LanguageSpec {
    definitions: r#"
(block_mapping_pair key: (flow_node) @name value: (block_node)) @def.module
(block_mapping_pair key: (flow_node) @name value: (flow_node)) @def.field
"#,
    references: "",
    routes: "",
    method_parents: &[],
    scope_kinds: &[],
};

/// Also the shape of a systemd unit, where `[Service]` is the section and
/// `ExecStart=` the setting.
static INI: LanguageSpec = LanguageSpec {
    definitions: r#"
(section (section_name (text) @name)) @def.module
(setting (setting_name) @name) @def.field
"#,
    references: "",
    routes: "",
    method_parents: &[],
    scope_kinds: &["section"],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_names_map_to_labels() {
        assert_eq!(label_for_capture("def.function"), Some(NodeLabel::Function));
        assert_eq!(label_for_capture("def.trait"), Some(NodeLabel::Trait));
        assert_eq!(label_for_capture("call"), None);
        assert_eq!(label_for_capture("def.nonsense"), None);
    }

    #[test]
    fn every_bundled_language_has_a_spec() {
        for entry in crate::registry::BUNDLED {
            assert!(
                spec_for(entry.language).is_some(),
                "{} is bundled but has no query spec",
                entry.language
            );
        }
    }
}
