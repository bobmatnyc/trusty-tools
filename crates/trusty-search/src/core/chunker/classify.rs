//! Node classification + name-extraction helpers for the AST chunker.
//!
//! Why: `chunker/mod.rs` was a 2200-line monolith mixing AST classification,
//! tree traversal, inheritance extraction, and per-format document chunkers.
//! Splitting the classification logic into its own module isolates the
//! per-grammar `(lang, node_kind) -> ChunkType` table — the part most likely
//! to need touching when a new grammar is added.
//! What: `classify_node` (the big `match` over languages) plus the small
//! ancestor-walking and qualified-name helpers used by `walk_for_chunks`.
//! Test: covered indirectly by every per-language test in `chunker/mod.rs`'s
//! `tests` module (Rust, Scala, PHP, Kotlin, Swift, C#, …).

use tree_sitter::Node;

use super::ChunkType;

/// Swift's tree-sitter grammar folds `struct`, `enum`, and `extension`
/// declarations into the same `class_declaration` node kind, distinguished
/// by the first child being the keyword token (kind `class`/`struct`/etc.).
/// Map that keyword to the appropriate `ChunkType`.
pub(super) fn swift_class_decl_kind(node: Node<'_>) -> ChunkType {
    let kw = node
        .child(0)
        .map(|c| c.kind().to_string())
        .unwrap_or_default();
    match kw.as_str() {
        "struct" => ChunkType::Struct,
        "enum" => ChunkType::Enum,
        "extension" => ChunkType::Module,
        _ => ChunkType::Class, // includes `class`, `actor`, fallback
    }
}

/// Per-language: AST node kinds we promote to top-level chunks, plus their
/// (default `ChunkType`, parent-context-overrides).
pub(super) fn classify_node(lang: &str, node: Node<'_>) -> Option<ChunkType> {
    let kind = node.kind();
    let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");
    Some(match (lang, kind) {
        ("rust", "function_item") => {
            // Method if inside `impl_item` / `trait_item`.
            if matches!(parent_kind, "declaration_list" | "impl_item" | "trait_item")
                || ancestor_kind(node, "impl_item").is_some()
            {
                ChunkType::Method
            } else {
                ChunkType::Function
            }
        }
        ("rust", "impl_item") => ChunkType::Impl,
        ("rust", "struct_item") => ChunkType::Class,
        ("rust", "trait_item") => ChunkType::Trait,
        ("rust", "enum_item") => ChunkType::Enum,
        ("rust", "mod_item") => ChunkType::Module,
        // `pub const NAME: TYPE = VALUE;` and `pub static NAME: TYPE = VALUE;`
        // (Phase 1: Rust only — see TODO below for other languages).
        // Only public declarations get their own Constant chunk; private consts
        // stay in the surrounding scope chunk so the chunk count doesn't
        // explode for internal implementation details.
        //
        // TODO: extend per-const chunking to Python (UPPERCASE module-level
        // assignments), TypeScript (`export const X = …`), Go (`const` /
        // `const ( … )` blocks), and Java (`public static final` fields) in
        // follow-up tickets once the Rust phase is validated.
        ("rust", "const_item") if rust_item_is_pub(node) => ChunkType::Constant,
        ("rust", "static_item") if rust_item_is_pub(node) => ChunkType::Constant,

        ("python", "function_definition") => {
            if ancestor_kind(node, "class_definition").is_some() {
                ChunkType::Method
            } else {
                ChunkType::Function
            }
        }
        ("python", "class_definition") => ChunkType::Class,
        ("python", "decorated_definition") => return None, // descend to inner

        ("javascript" | "typescript", "function_declaration") => ChunkType::Function,
        ("javascript" | "typescript", "class_declaration") => ChunkType::Class,
        ("javascript" | "typescript", "method_definition") => ChunkType::Method,

        ("go", "function_declaration") => ChunkType::Function,
        ("go", "method_declaration") => ChunkType::Method,
        ("go", "type_declaration") => ChunkType::Class,

        ("java", "method_declaration") => ChunkType::Method,
        ("java", "class_declaration") => ChunkType::Class,
        ("java", "interface_declaration") => ChunkType::Trait,

        ("c" | "cpp", "function_definition") => ChunkType::Function,
        ("cpp", "class_specifier") => ChunkType::Class,
        ("c" | "cpp", "struct_specifier") => ChunkType::Class,

        ("ruby", "method") => ChunkType::Function,
        ("ruby", "singleton_method") => ChunkType::Method,
        ("ruby", "module") => ChunkType::Module,
        ("ruby", "class") => ChunkType::Class,

        ("php", "function_definition") => ChunkType::Function,
        ("php", "method_declaration") => ChunkType::Method,
        ("php", "class_declaration") => ChunkType::Class,
        ("php", "interface_declaration") => ChunkType::Trait,
        ("php", "trait_declaration") => ChunkType::Trait,
        ("php", "namespace_definition") => ChunkType::Module,

        ("scala", "function_definition") => {
            // Phase 2 (issue #55): a `def` inside a class/object/trait template
            // body is a method; standalone defs (top-level or inside another
            // function) are plain functions.
            if ancestor_kind(node, "class_definition").is_some()
                || ancestor_kind(node, "object_definition").is_some()
                || ancestor_kind(node, "trait_definition").is_some()
            {
                ChunkType::Method
            } else {
                ChunkType::Function
            }
        }
        ("scala", "class_definition") => ChunkType::Class,
        ("scala", "object_definition") => ChunkType::Class,
        ("scala", "trait_definition") => ChunkType::Trait,

        // C#
        ("csharp", "method_declaration") => {
            if ancestor_kind(node, "class_declaration").is_some()
                || ancestor_kind(node, "interface_declaration").is_some()
                || ancestor_kind(node, "struct_declaration").is_some()
            {
                ChunkType::Method
            } else {
                ChunkType::Function
            }
        }
        ("csharp", "constructor_declaration") => ChunkType::Method,
        ("csharp", "class_declaration") => ChunkType::Class,
        ("csharp", "interface_declaration") => ChunkType::Trait,
        ("csharp", "struct_declaration") => ChunkType::Class,
        ("csharp", "namespace_declaration") => ChunkType::Module,
        ("csharp", "enum_declaration") => ChunkType::Enum,

        // Kotlin (tree-sitter-kotlin-ng grammar)
        ("kotlin", "function_declaration") => {
            if ancestor_kind(node, "class_declaration").is_some()
                || ancestor_kind(node, "object_declaration").is_some()
            {
                ChunkType::Method
            } else {
                ChunkType::Function
            }
        }
        ("kotlin", "secondary_constructor") => ChunkType::Method,
        ("kotlin", "class_declaration") => ChunkType::Class,
        ("kotlin", "object_declaration") => ChunkType::Class,
        ("kotlin", "companion_object") => ChunkType::Class,
        ("kotlin", "interface_declaration") => ChunkType::Trait,

        // Swift: tree-sitter-swift folds struct/enum/extension into
        // `class_declaration`, distinguished by the keyword token at child(0).
        ("swift", "class_declaration") => swift_class_decl_kind(node),
        ("swift", "protocol_declaration") => ChunkType::Trait,
        ("swift", "function_declaration") | ("swift", "protocol_function_declaration") => {
            if ancestor_kind(node, "class_declaration").is_some()
                || ancestor_kind(node, "protocol_declaration").is_some()
            {
                ChunkType::Method
            } else {
                ChunkType::Function
            }
        }
        ("swift", "init_declaration") => ChunkType::Method,

        _ => return None,
    })
}

/// Walk up `node`'s ancestor chain looking for the first node of `kind`.
pub(super) fn ancestor_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cur = node.parent();
    while let Some(c) = cur {
        if c.kind() == kind {
            return Some(c);
        }
        cur = c.parent();
    }
    None
}

/// Like `ancestor_kind`, but accepts a slice of candidate node kinds.
///
/// Why: Scala and PHP each have several container kinds (class/object/trait
/// for Scala; class/interface/trait for PHP) that can own a method. A single
/// `ancestor_kind` call per kind would do three linear walks; this collapses
/// them into one.
/// What: walks up from `node` and returns the first ancestor whose kind appears
/// in `kinds`.
/// Test: indirectly covered by `test_scala_method_qualified_name` and
/// `test_php_method_qualified_name`.
pub(super) fn ancestor_kind_any<'a>(node: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    let mut cur = node.parent();
    while let Some(c) = cur {
        if kinds.contains(&c.kind()) {
            return Some(c);
        }
        cur = c.parent();
    }
    None
}

/// Find the first `name` field child of an AST node (works across most
/// tree-sitter grammars where declarations expose a `name` field).
pub(super) fn name_of(node: Node<'_>, src: &[u8]) -> String {
    if let Some(n) = node.child_by_field_name("name") {
        return std::str::from_utf8(&src[n.start_byte()..n.end_byte()])
            .unwrap_or("")
            .to_string();
    }
    String::new()
}

/// For Rust methods: walk up to the enclosing `impl_item` and grab its `type` field.
pub(super) fn rust_impl_type_name(node: Node<'_>, src: &[u8]) -> Option<String> {
    let imp = ancestor_kind(node, "impl_item")?;
    let t = imp.child_by_field_name("type")?;
    Some(
        std::str::from_utf8(&src[t.start_byte()..t.end_byte()])
            .unwrap_or("")
            .to_string(),
    )
}

/// For Scala methods (issue #55): walk up to the enclosing
/// `class_definition` / `object_definition` / `trait_definition` and grab its
/// `name` field so the method can be qualified as `ClassName::methodName`.
///
/// Why: Phase 2 caller-scoped call edges need stable, container-qualified
/// symbol names. Without qualification, `Foo.bar` and `Baz.bar` collide in the
/// symbol graph and `callers_of("bar")` returns false positives.
/// What: returns the first ancestor template owner's `name` field text, or
/// None for top-level / standalone defs.
/// Test: covered by `test_scala_method_qualified_name`.
pub(super) fn scala_enclosing_class_name(node: Node<'_>, src: &[u8]) -> Option<String> {
    let owner = ancestor_kind_any(
        node,
        &["class_definition", "object_definition", "trait_definition"],
    )?;
    let n = owner.child_by_field_name("name")?;
    Some(
        std::str::from_utf8(&src[n.start_byte()..n.end_byte()])
            .unwrap_or("")
            .to_string(),
    )
}

/// Return `true` when a Rust `const_item` or `static_item` node has a
/// `visibility_modifier` child (i.e. is `pub`, `pub(crate)`, etc.).
///
/// Why: Phase 1 per-const chunking is scoped to public declarations only.
/// Private constants stay in whatever surrounding scope chunk would otherwise
/// claim them, avoiding an explosion of trivial implementation-detail chunks.
/// What: walks the direct children of `node` looking for a
/// `visibility_modifier` node; returns `true` on first hit, `false` otherwise.
/// Test: `test_rust_pub_const_chunking` verifies that `pub const` items get
/// Constant chunks and private `const` items do not.
pub(super) fn rust_item_is_pub(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            return true;
        }
    }
    false
}

/// For PHP methods (issue #49): walk up to the enclosing
/// `class_declaration` / `interface_declaration` / `trait_declaration` and
/// grab its `name` field so the method can be qualified as
/// `ClassName::methodName`.
///
/// Why: same rationale as `scala_enclosing_class_name` — symbol-graph
/// uniqueness across classes that share method names like `handle` or `run`.
/// What: returns the first ancestor declaration's `name` field text, or None
/// for free functions.
/// Test: covered by `test_php_method_qualified_name`.
pub(super) fn php_enclosing_class_name(node: Node<'_>, src: &[u8]) -> Option<String> {
    let owner = ancestor_kind_any(
        node,
        &[
            "class_declaration",
            "interface_declaration",
            "trait_declaration",
        ],
    )?;
    let n = owner.child_by_field_name("name")?;
    Some(
        std::str::from_utf8(&src[n.start_byte()..n.end_byte()])
            .unwrap_or("")
            .to_string(),
    )
}
