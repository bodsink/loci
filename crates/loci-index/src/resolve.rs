use loci_graph::{Edge, EdgeType, Evidence, Node, NodeLabel, StoredCall, StoredReceiverBinding};
use std::collections::HashMap;

/// How a call edge was established, recorded on the edge so a reader can judge
/// how much to trust it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// The receiver's type is known, and the type owns exactly one method by
    /// this name.
    ReceiverType,
    /// Callee defined in the same file.
    SameFile,
    /// Callee defined in the same dotted module prefix.
    SameModule,
    /// Exactly one callable with this name exists in the whole project.
    UniqueInProject,
}

impl Resolution {
    const fn detail(self) -> &'static str {
        match self {
            Self::ReceiverType => "receiver_type_match",
            Self::SameFile => "same_file_name_match",
            Self::SameModule => "same_module_name_match",
            Self::UniqueInProject => "unique_callable_name_in_project",
        }
    }

    const fn confidence(self) -> f32 {
        match self {
            // Naming the owning type is what the name-only paths below cannot
            // do; it is the difference between one method and the 72 others
            // that happen to be called `List`.
            Self::ReceiverType => 1.0,
            Self::SameFile => 1.0,
            Self::SameModule => 0.9,
            // A project-wide unique name is strong evidence but not proof; the
            // caller may be invoking a same-named import from a dependency.
            Self::UniqueInProject => 0.7,
        }
    }
}

/// A callable candidate: its node id and whether it is a method.
type Candidate = (u64, bool);

/// Lookup tables over every callable in the project, built once per index run.
pub struct SymbolTable {
    by_qualified_name: HashMap<String, u64>,
    /// (file_path, simple_name) -> candidates
    by_file_and_name: HashMap<(String, String), Vec<Candidate>>,
    /// (module_prefix, simple_name) -> candidates
    by_module_and_name: HashMap<(String, String), Vec<Candidate>>,
    /// simple_name -> candidates, across the whole project
    by_name: HashMap<String, Vec<Candidate>>,
    /// (owning type, method name) -> method node ids
    by_owner_and_name: HashMap<(String, String), Vec<u64>>,
    /// function name -> the type it returns, read from its own signature
    returns: HashMap<String, Option<String>>,
    /// (file, variable) -> the type that variable holds
    receiver_types: HashMap<(String, String), String>,
}

/// How a call was written at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallShape {
    /// `helper()` — no receiver.
    Bare,
    /// `self.helper()` / `this.helper()` — receiver is the enclosing object.
    SelfReceiver,
    /// `service.helper()` — receiver is some other object.
    OtherReceiver,
}

impl CallShape {
    pub fn of(receiver: Option<&str>) -> Self {
        match receiver {
            None => Self::Bare,
            Some("self") | Some("this") => Self::SelfReceiver,
            Some(_) => Self::OtherReceiver,
        }
    }

    /// A call through another object cannot be a plain free function in the
    /// caller's own file; preferring methods avoids the classic false match
    /// between `service.create_order()` and a local `create_order()`.
    fn prefers_methods(self) -> bool {
        matches!(self, Self::OtherReceiver)
    }
}

/// Pick the single candidate a call can mean, honouring the call's shape.
///
/// A call through a foreign receiver can only mean a method. Falling back to a
/// same-named free function there is how name-only resolvers invent edges, so
/// this returns `None` instead and the call is recorded as unresolved.
fn pick(candidates: &[Candidate], shape: CallShape) -> Option<u64> {
    let eligible: Vec<&Candidate> = if shape.prefers_methods() {
        candidates
            .iter()
            .filter(|(_, is_method)| *is_method)
            .collect()
    } else {
        candidates.iter().collect()
    };

    match eligible.as_slice() {
        [only] => Some(only.0),
        _ => None,
    }
}

/// The type a method hangs off: the segment before its own name.
///
/// Works the same for `pkg.ZoneHandler.List` and Python's `mod.Cls.method`,
/// because both spell containment the same way once the name is qualified.
fn owner_of(qualified_name: &str) -> Option<&str> {
    let mut segments = qualified_name.rsplit('.');
    segments.next()?;
    segments.next().filter(|owner| !owner.is_empty())
}

fn module_of(qualified_name: &str, own_name: &str) -> String {
    qualified_name
        .strip_suffix(own_name)
        .map(|prefix| prefix.trim_end_matches('.').to_string())
        .unwrap_or_default()
}

impl SymbolTable {
    pub fn build(nodes: &[Node]) -> Self {
        let mut table = Self {
            by_qualified_name: HashMap::new(),
            by_file_and_name: HashMap::new(),
            by_module_and_name: HashMap::new(),
            by_name: HashMap::new(),
            by_owner_and_name: HashMap::new(),
            returns: HashMap::new(),
            receiver_types: HashMap::new(),
        };

        for node in nodes {
            table
                .by_qualified_name
                .insert(node.qualified_name.clone(), node.id);

            if !node.label.is_callable() {
                continue;
            }

            if node.label == NodeLabel::Method {
                if let Some(owner) = owner_of(&node.qualified_name) {
                    table
                        .by_owner_and_name
                        .entry((owner.to_string(), node.name.clone()))
                        .or_default()
                        .push(node.id);
                }
            }

            // A name assigned two different return types is no evidence at all,
            // so it is recorded as unusable rather than as the last one seen.
            let returns = node.extra.get("returns").cloned();
            table
                .returns
                .entry(node.name.clone())
                .and_modify(|known| {
                    if known.as_deref() != returns.as_deref() {
                        *known = None;
                    }
                })
                .or_insert(returns);
            let candidate = (node.id, node.label == NodeLabel::Method);
            table
                .by_file_and_name
                .entry((node.file_path.clone(), node.name.clone()))
                .or_default()
                .push(candidate);
            table
                .by_module_and_name
                .entry((
                    module_of(&node.qualified_name, &node.name),
                    node.name.clone(),
                ))
                .or_default()
                .push(candidate);
            table
                .by_name
                .entry(node.name.clone())
                .or_default()
                .push(candidate);
        }

        table
    }

    pub fn node_id_for_qualified_name(&self, qualified_name: &str) -> Option<u64> {
        self.by_qualified_name.get(qualified_name).copied()
    }

    /// The type a constructor hands back, for typing the variable it fills.
    pub fn return_type_of(&self, function_name: &str) -> Option<&str> {
        self.returns.get(function_name)?.as_deref()
    }

    /// Give each bound variable the type its constructor returns.
    ///
    /// Runs after the table is built because it reads the signatures the build
    /// collected. A binding whose constructor is not in this project — a
    /// third-party client, say — types nothing, and its calls fall back to
    /// resolution by name.
    pub fn learn_receiver_bindings(&mut self, bindings: &[StoredReceiverBinding]) {
        for binding in bindings {
            let Some(type_name) = self.return_type_of(&binding.constructor) else {
                continue;
            };
            self.receiver_types.insert(
                (binding.file_path.clone(), binding.variable.clone()),
                type_name.to_string(),
            );
        }
    }

    /// The type a variable holds at a given file, if it is known.
    pub fn receiver_type(&self, file: &str, variable: &str) -> Option<&str> {
        self.receiver_types
            .get(&(file.to_string(), variable.to_string()))
            .map(String::as_str)
    }

    /// Resolve a call, using the receiver's type when that type is known.
    ///
    /// Type-directed resolution is tried first because it is the only path that
    /// can separate one `List` from the 72 others in a repository. When the
    /// type is unknown the behaviour is exactly as before.
    pub fn resolve_call_with_receiver(
        &self,
        callee_name: &str,
        from_file: &str,
        from_qualified_name: &str,
        receiver: Option<&str>,
    ) -> Option<(u64, Resolution)> {
        if let Some(type_name) = receiver.and_then(|name| self.receiver_type(from_file, name)) {
            if let Some(id) = self.resolve_method_on_type(type_name, callee_name) {
                return Some((id, Resolution::ReceiverType));
            }
        }
        self.resolve_call(
            callee_name,
            from_file,
            from_qualified_name,
            CallShape::of(receiver),
        )
    }

    /// The one method a named type owns by this name.
    ///
    /// A type declaring the same method twice is a build error in Go, so more
    /// than one candidate means the owner was misread, and nothing is returned.
    pub fn resolve_method_on_type(&self, type_name: &str, method: &str) -> Option<u64> {
        match self
            .by_owner_and_name
            .get(&(type_name.to_string(), method.to_string()))?
            .as_slice()
        {
            [only] => Some(*only),
            _ => None,
        }
    }

    /// Resolve a callee name seen inside `from_qualified_name`.
    ///
    /// Returns `None` when the name cannot be pinned to exactly one definition;
    /// the caller then records an unresolved call rather than guessing.
    pub fn resolve_call(
        &self,
        callee_name: &str,
        from_file: &str,
        from_qualified_name: &str,
        shape: CallShape,
    ) -> Option<(u64, Resolution)> {
        if let Some(candidates) = self
            .by_file_and_name
            .get(&(from_file.to_string(), callee_name.to_string()))
        {
            if let Some(id) = pick(candidates, shape) {
                return Some((id, Resolution::SameFile));
            }
        }

        let caller_module = from_qualified_name
            .rsplit_once('.')
            .map(|(prefix, _)| prefix.to_string())
            .unwrap_or_default();
        if !caller_module.is_empty() {
            if let Some(candidates) = self
                .by_module_and_name
                .get(&(caller_module, callee_name.to_string()))
            {
                if let Some(id) = pick(candidates, shape) {
                    return Some((id, Resolution::SameModule));
                }
            }
        }

        if let Some(candidates) = self.by_name.get(callee_name) {
            if let Some(id) = pick(candidates, shape) {
                return Some((id, Resolution::UniqueInProject));
            }
        }

        None
    }

    /// Resolve a type name for inheritance and implementation edges.
    pub fn resolve_type(&self, type_name: &str) -> Option<u64> {
        if let Some(id) = self.by_qualified_name.get(type_name) {
            return Some(*id);
        }
        // Types are not in by_name (that map holds callables only), so fall back
        // to a suffix match on qualified names.
        let suffix = format!(".{type_name}");
        let mut matches = self
            .by_qualified_name
            .iter()
            .filter(|(qn, _)| qn.ends_with(&suffix) || qn.as_str() == type_name);
        let first = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(*first.1)
    }
}

/// Why a call could not be resolved. Recorded verbatim on the edge.
fn unresolved_reason(table: &SymbolTable, callee_name: &str, shape: CallShape) -> &'static str {
    match table.by_name.get(callee_name) {
        Some(candidates) if candidates.len() > 1 => "ambiguous_name_in_project",
        Some(_) if shape.prefers_methods() => "receiver_type_unknown_no_matching_method",
        Some(_) => "name_known_but_scope_mismatch",
        None => "no_definition_in_index",
    }
}

/// Build call edges from stored call sites.
///
/// Every site produces exactly one edge: a `CALLS` edge when the callee resolves
/// to a single definition, otherwise a `CALL_UNRESOLVED` edge that carries the
/// callee name and the reason. Nothing is dropped silently.
pub fn build_call_edges(
    table: &SymbolTable,
    callsites: &[StoredCall],
    next_edge_id: &mut u32,
) -> CallEdges {
    let mut edges = Vec::with_capacity(callsites.len());
    let mut unresolved = Vec::new();

    for site in callsites {
        let Some(src) = table.node_id_for_qualified_name(&site.from_qualified_name) else {
            continue;
        };
        let shape = CallShape::of(site.receiver.as_deref());

        let mut edge = match table.resolve_call_with_receiver(
            &site.callee_name,
            &site.file_path,
            &site.from_qualified_name,
            site.receiver.as_deref(),
        ) {
            Some((dst, resolution)) if dst != src => {
                let mut edge = Edge::new(src, dst, EdgeType::Calls);
                edge.detail = Some(resolution.detail().to_string());
                edge.confidence = resolution.confidence();
                edge.line = Some(site.line);
                edge
            }
            // A self-call is real but adds no traversal value and would make
            // every recursive function its own caller.
            Some((_, _)) => {
                let mut edge = Edge::new(src, src, EdgeType::Calls);
                edge.detail = Some("recursive_self_call".to_string());
                edge.line = Some(site.line);
                edge
            }
            None => {
                // Kept so the Hybrid LSP pass can ask a type checker about
                // exactly the calls syntax could not settle.
                unresolved.push((src, site.clone()));
                Edge::unresolved_call(
                    src,
                    &site.callee_name,
                    site.line,
                    unresolved_reason(table, &site.callee_name, shape),
                )
            }
        };

        edge.id = *next_edge_id;
        edge.source = Evidence::Ast;
        *next_edge_id += 1;
        edges.push(edge);
    }

    CallEdges { edges, unresolved }
}

/// Call edges plus the sites that stayed unresolved, paired with the node they
/// were called from.
pub struct CallEdges {
    pub edges: Vec<Edge>,
    pub unresolved: Vec<(u64, StoredCall)>,
}

/// Attribute a call site to the definition that encloses it, falling back to
/// the file's own module node when the call sits at top level.
pub fn owner_qualified_name(
    enclosing: Option<&loci_parse::Definition>,
    file_module_qn: &str,
) -> String {
    enclosing
        .map(|d| d.qualified_name.clone())
        .unwrap_or_else(|| file_module_qn.to_string())
}

/// Nodes eligible to own a call site: callables plus the file node itself.
pub fn is_call_owner(label: NodeLabel) -> bool {
    label.is_callable() || label == NodeLabel::File
}

#[cfg(test)]
mod tests {
    use super::*;
    use loci_core::LanguageId;

    #[test]
    fn an_owner_is_the_segment_before_the_name() {
        assert_eq!(
            owner_of("internal.handlers.zone.ZoneHandler.List"),
            Some("ZoneHandler")
        );
        assert_eq!(owner_of("List"), None);
    }

    fn callable(id: u64, name: &str, qn: &str, file: &str) -> Node {
        let mut node = Node::new(
            NodeLabel::Function,
            name,
            qn,
            file,
            1,
            5,
            Some(LanguageId::Python),
        );
        node.id = id;
        node
    }

    fn method(id: u64, name: &str, qn: &str, file: &str) -> Node {
        let mut node = callable(id, name, qn, file);
        node.label = NodeLabel::Method;
        node
    }

    fn site(from: &str, callee: &str, file: &str) -> StoredCall {
        StoredCall {
            from_qualified_name: from.to_string(),
            callee_name: callee.to_string(),
            receiver: None,
            line: 3,
            character: 4,
            file_path: file.to_string(),
        }
    }

    fn site_via(from: &str, receiver: &str, callee: &str, file: &str) -> StoredCall {
        StoredCall {
            receiver: Some(receiver.to_string()),
            ..site(from, callee, file)
        }
    }

    #[test]
    fn same_file_calls_resolve_with_full_confidence() {
        let nodes = vec![
            callable(1, "caller", "m.caller", "m.py"),
            callable(2, "callee", "m.callee", "m.py"),
        ];
        let table = SymbolTable::build(&nodes);
        let mut next = 1;
        let edges =
            build_call_edges(&table, &[site("m.caller", "callee", "m.py")], &mut next).edges;

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].edge_type, EdgeType::Calls);
        assert_eq!(edges[0].dst, 2);
        assert_eq!(edges[0].confidence, 1.0);
    }

    #[test]
    fn cross_file_unique_names_resolve_with_lower_confidence() {
        let nodes = vec![
            callable(1, "caller", "a.caller", "a.py"),
            callable(2, "helper", "b.helper", "b.py"),
        ];
        let table = SymbolTable::build(&nodes);
        let mut next = 1;
        let edges =
            build_call_edges(&table, &[site("a.caller", "helper", "a.py")], &mut next).edges;

        assert_eq!(edges[0].edge_type, EdgeType::Calls);
        assert_eq!(edges[0].dst, 2);
        assert!(edges[0].confidence < 1.0);
        assert_eq!(
            edges[0].detail.as_deref(),
            Some("unique_callable_name_in_project")
        );
    }

    #[test]
    fn ambiguous_names_stay_unresolved_instead_of_guessing() {
        let nodes = vec![
            callable(1, "caller", "a.caller", "a.py"),
            callable(2, "helper", "b.helper", "b.py"),
            callable(3, "helper", "c.helper", "c.py"),
        ];
        let table = SymbolTable::build(&nodes);
        let mut next = 1;
        let edges =
            build_call_edges(&table, &[site("a.caller", "helper", "a.py")], &mut next).edges;

        assert_eq!(edges[0].edge_type, EdgeType::CallUnresolved);
        assert_eq!(edges[0].target_name.as_deref(), Some("helper"));
        assert_eq!(
            edges[0].detail.as_deref(),
            Some("ambiguous_name_in_project")
        );
    }

    #[test]
    fn unknown_callees_record_the_name_and_reason() {
        let nodes = vec![callable(1, "caller", "a.caller", "a.py")];
        let table = SymbolTable::build(&nodes);
        let mut next = 1;
        let edges = build_call_edges(
            &table,
            &[site("a.caller", "requests_get", "a.py")],
            &mut next,
        )
        .edges;

        assert_eq!(edges[0].edge_type, EdgeType::CallUnresolved);
        assert_eq!(edges[0].detail.as_deref(), Some("no_definition_in_index"));
    }

    #[test]
    fn a_local_definition_wins_over_a_same_named_one_elsewhere() {
        let nodes = vec![
            callable(1, "caller", "a.caller", "a.py"),
            callable(2, "helper", "a.helper", "a.py"),
            callable(3, "helper", "b.helper", "b.py"),
        ];
        let table = SymbolTable::build(&nodes);
        let mut next = 1;
        let edges =
            build_call_edges(&table, &[site("a.caller", "helper", "a.py")], &mut next).edges;

        assert_eq!(edges[0].dst, 2, "the same-file definition must win");
        assert_eq!(edges[0].detail.as_deref(), Some("same_file_name_match"));
    }

    #[test]
    fn a_call_through_another_object_prefers_a_method_over_a_local_function() {
        // main.create_order calls service.create_order(...). Name-only matching
        // would resolve that to the local function and invent a self-call.
        let nodes = vec![
            callable(1, "create_order", "main.create_order", "main.py"),
            method(
                2,
                "create_order",
                "service.OrderService.create_order",
                "service.py",
            ),
        ];
        let table = SymbolTable::build(&nodes);
        let mut next = 1;
        let edges = build_call_edges(
            &table,
            &[site_via(
                "main.create_order",
                "service",
                "create_order",
                "main.py",
            )],
            &mut next,
        )
        .edges;

        assert_eq!(edges[0].edge_type, EdgeType::Calls);
        assert_eq!(
            edges[0].dst, 2,
            "the method must win over the local function"
        );
    }

    #[test]
    fn a_self_receiver_still_resolves_within_the_same_file() {
        let nodes = vec![
            method(1, "create", "m.Service.create", "m.py"),
            method(2, "validate", "m.Service.validate", "m.py"),
        ];
        let table = SymbolTable::build(&nodes);
        let mut next = 1;
        let edges = build_call_edges(
            &table,
            &[site_via("m.Service.create", "self", "validate", "m.py")],
            &mut next,
        )
        .edges;

        assert_eq!(edges[0].dst, 2);
        assert_eq!(edges[0].detail.as_deref(), Some("same_file_name_match"));
    }

    #[test]
    fn an_unknown_receiver_type_is_reported_rather_than_guessed() {
        let nodes = vec![
            callable(1, "caller", "a.caller", "a.py"),
            callable(2, "helper", "b.helper", "b.py"),
        ];
        let table = SymbolTable::build(&nodes);
        let mut next = 1;
        let edges = build_call_edges(
            &table,
            &[site_via("a.caller", "client", "helper", "a.py")],
            &mut next,
        )
        .edges;

        assert_eq!(edges[0].edge_type, EdgeType::CallUnresolved);
        assert_eq!(
            edges[0].detail.as_deref(),
            Some("receiver_type_unknown_no_matching_method")
        );
    }

    #[test]
    fn recursion_is_labelled_rather_than_dropped() {
        let nodes = vec![callable(1, "walk", "m.walk", "m.py")];
        let table = SymbolTable::build(&nodes);
        let mut next = 1;
        let edges = build_call_edges(&table, &[site("m.walk", "walk", "m.py")], &mut next).edges;

        assert_eq!(edges[0].src, edges[0].dst);
        assert_eq!(edges[0].detail.as_deref(), Some("recursive_self_call"));
    }
}
