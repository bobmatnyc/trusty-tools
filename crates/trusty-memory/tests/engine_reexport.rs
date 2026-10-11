//! `trusty_memory::engine` is `trusty_common::memory_core` (#9270).
//!
//! Why: consumers move their imports from `trusty_common::memory_core` to
//! `trusty_memory::engine` before the engine physically moves (#9271). That
//! move is safe only if both paths name the same items, so a value of one
//! type passes where the other is expected.
//! What: each function below takes an item through one path and returns it
//! through the other. The file compiles only when the paths are one module.
//! It needs no feature, so it also runs under `--no-default-features`.
//! Test: this file.

use trusty_common::memory_core as core_path;
use trusty_memory::engine as engine_path;

fn drawer_round_trip(drawer: core_path::palace::Drawer) -> engine_path::palace::Drawer {
    drawer
}

fn registry_round_trip(registry: engine_path::PalaceRegistry) -> core_path::PalaceRegistry {
    registry
}

#[test]
fn engine_is_the_trusty_common_memory_core_module() {
    let drawer = core_path::palace::Drawer::new(uuid::Uuid::new_v4(), "same type");
    let id = drawer.id;
    assert_eq!(drawer_round_trip(drawer).id, id);

    let _: fn(engine_path::PalaceRegistry) -> core_path::PalaceRegistry = registry_round_trip;

    // The mock seam trusty-crate-contracts calls resolves through `engine`.
    engine_path::retrieval::seed_shared_embedder_with_mock();
}
