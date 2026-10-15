//! trusty-common no longer carries the Memory Palace engine (#9271).
//!
//! Why: the engine moved to trusty-memory as `trusty_memory::engine`. Its
//! heavy crates must leave this manifest with it, or every trusty-common
//! consumer keeps paying for them through a feature that now enables nothing.
//! What: reads this crate's `Cargo.toml` and checks that no dependency table
//! names a crate only the engine used, that `memory-core` enables nothing, and
//! that `src/memory_core` is gone.
//! Test: this file.

use toml::Value;

/// Crates only the engine used. Each left with it.
const ENGINE_ONLY: &[&str] = &[
    "hnsw_rs",
    "git2",
    "postcard",
    "dashmap",
    "unicode-normalization",
];

fn manifest() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    text.parse::<Value>()
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// Every dependency table: the three top-level ones and each
/// `[target.'cfg(..)'.*]` copy.
fn dependency_tables(manifest: &Value) -> Vec<(String, &toml::Table)> {
    const KINDS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
    let mut out = Vec::new();
    for kind in KINDS {
        if let Some(table) = manifest.get(kind).and_then(Value::as_table) {
            out.push((kind.to_string(), table));
        }
    }
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for (cfg, body) in targets {
            for kind in KINDS {
                if let Some(table) = body.get(kind).and_then(Value::as_table) {
                    out.push((format!("target.{cfg}.{kind}"), table));
                }
            }
        }
    }
    out
}

/// Why (#9271): AC1 — the engine's crates are gone from this manifest.
/// What: fails naming each table that still lists an [`ENGINE_ONLY`] crate.
/// The `[dependencies]` table must be found, so an empty read cannot pass.
/// Test: itself.
#[test]
fn no_dependency_table_names_an_engine_only_crate() {
    let manifest = manifest();
    let tables = dependency_tables(&manifest);
    assert!(
        tables.iter().any(|(kind, _)| kind == "dependencies"),
        "no [dependencies] table found"
    );
    let hits: Vec<String> = tables
        .iter()
        .flat_map(|(kind, table)| {
            ENGINE_ONLY
                .iter()
                .filter(|name| table.contains_key(**name))
                .map(move |name| format!("{kind}: {name}"))
        })
        .collect();
    assert!(
        hits.is_empty(),
        "crates only the engine used are still listed: {hits:?}"
    );
}

/// Why (#9271, D2): `memory-core` stays so old manifests resolve, but it must
/// turn nothing on, and the module it gated must be gone.
/// What: checks `features.memory-core == []` and that `src/memory_core` does
/// not exist.
/// Test: itself.
#[test]
fn memory_core_is_a_no_op_and_the_module_is_gone() {
    let manifest = manifest();
    let feature = manifest
        .get("features")
        .and_then(|f| f.get("memory-core"))
        .and_then(Value::as_array)
        .expect("`memory-core` is still declared, as a list");
    assert!(
        feature.is_empty(),
        "`memory-core` must enable nothing: {feature:?}"
    );
    let module = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/memory_core");
    assert!(!module.exists(), "{} still exists", module.display());
}
