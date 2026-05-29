//! Topological repair of wave orderings (#162).
//!
//! Why: Plan-agents sometimes emit `assignments.json` where a file and its
//! dependency share a wave (or the dep is in a later wave). Rejecting the whole
//! plan defeats the wave loop, so before rejection we attempt Kahn's algorithm
//! to re-layer the files. Isolating that ~140-line graph routine (#359) keeps
//! `assignments.rs` under the 500-line cap.
//! What: `Assignments::repair_wave_ordering` rebuilds `self.waves` from a
//! longest-path layer assignment over the dependency DAG.
//! Test: `wave_validator_repairs_same_wave_dep`, `wave_validator_rejects_true_cycle`,
//! `wave_validator_handles_conftest_pattern` in the parent `tests` submodule.

use super::{Assignments, FileAssignment, WaveDef};

impl Assignments {
    /// Repair same-wave / forward dependency violations by topologically
    /// re-assigning files to waves (#162).
    ///
    /// Why: Plan-agents sometimes emit plans where `conftest.py` and the
    /// `test_*.py` files that import it live in the same wave. Rejecting the
    /// entire plan forces the engine to fall back to the legacy monolithic
    /// code phase, which defeats the wave loop for L1–L5. Before rejection,
    /// attempt Kahn's algorithm: if the dependency graph is a DAG, we can
    /// always produce a valid layered ordering.
    /// What: Builds the dependency graph across ALL files in all waves,
    /// runs Kahn's BFS to compute the longest-path layer for each node, and
    /// rewrites `self.waves` accordingly. Returns `Ok(true)` when any file
    /// moved, `Ok(false)` when the existing layering was already valid, and
    /// `Err(_)` when a true cycle prevents linearization (or the graph
    /// references an unknown dependency — caller should treat that as fatal).
    /// Test: `wave_validator_repairs_same_wave_dep`,
    /// `wave_validator_rejects_true_cycle`,
    /// `wave_validator_handles_conftest_pattern`.
    pub fn repair_wave_ordering(&mut self) -> Result<bool, String> {
        use std::collections::{HashMap, HashSet, VecDeque};

        // Collect every file path and its dependencies + per-file assignment.
        // We keep the original insertion order so that ties in layer
        // assignment produce a deterministic output aligned with the input.
        let mut order: Vec<String> = Vec::new();
        let mut deps: HashMap<String, Vec<String>> = HashMap::new();
        let mut original_layer: HashMap<String, u32> = HashMap::new();
        let mut files_by_path: HashMap<String, FileAssignment> = HashMap::new();
        let mut known_paths: HashSet<String> = HashSet::new();

        for wave in &self.waves {
            for f in &wave.files {
                known_paths.insert(f.path.clone());
            }
        }

        for wave in &self.waves {
            for f in &wave.files {
                order.push(f.path.clone());
                deps.insert(f.path.clone(), f.depends_on.clone());
                original_layer.insert(f.path.clone(), wave.wave);
                files_by_path.insert(f.path.clone(), f.clone());
            }
        }

        // Reject early if any dep references an unknown path — we cannot
        // manufacture a node for it.
        for (path, dlist) in &deps {
            for d in dlist {
                if !known_paths.contains(d) {
                    return Err(format!(
                        "file '{path}' depends on '{d}' which is not declared in any wave"
                    ));
                }
            }
        }

        // Build in-degree and reverse-adjacency for Kahn's algorithm.
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        let mut dependents: HashMap<String, Vec<String>> = HashMap::new();
        for path in &order {
            in_degree.insert(path.clone(), 0);
            dependents.insert(path.clone(), Vec::new());
        }
        for (path, dlist) in &deps {
            for d in dlist {
                // path depends on d → d must come first. Edge d -> path.
                *in_degree.get_mut(path).unwrap() += 1;
                dependents.get_mut(d).unwrap().push(path.clone());
            }
        }

        // Kahn's algorithm with layer assignment: layer(node) = 1 + max(layer(deps)).
        let mut layer: HashMap<String, u32> = HashMap::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        for path in &order {
            if in_degree[path] == 0 {
                layer.insert(path.clone(), 1);
                queue.push_back(path.clone());
            }
        }

        let mut processed = 0usize;
        while let Some(node) = queue.pop_front() {
            processed += 1;
            let node_layer = layer[&node];
            // Clone dependent list to avoid borrow conflicts.
            let deps_of_node = dependents[&node].clone();
            for dep in deps_of_node {
                let cur = in_degree.get_mut(&dep).unwrap();
                *cur -= 1;
                let candidate_layer = node_layer + 1;
                let existing = layer.get(&dep).copied().unwrap_or(0);
                if candidate_layer > existing {
                    layer.insert(dep.clone(), candidate_layer);
                }
                if *cur == 0 {
                    queue.push_back(dep);
                }
            }
        }

        if processed != order.len() {
            // Unprocessed nodes → cycle.
            let unresolved: Vec<String> = order
                .iter()
                .filter(|p| in_degree.get(*p).copied().unwrap_or(0) > 0)
                .cloned()
                .collect();
            return Err(format!(
                "dependency cycle detected; cannot linearize {} file(s): {}",
                unresolved.len(),
                unresolved.join(", ")
            ));
        }

        // Determine if repair actually changes anything.
        let mut changed = false;
        for path in &order {
            if original_layer[path] != layer[path] {
                changed = true;
                break;
            }
        }

        if !changed {
            return Ok(false);
        }

        // Rebuild self.waves from the layer map, preserving the original
        // per-layer insertion order so the output is deterministic.
        let max_layer = layer.values().copied().max().unwrap_or(0);
        let mut new_waves: Vec<WaveDef> = (1..=max_layer)
            .map(|w| WaveDef {
                wave: w,
                files: Vec::new(),
            })
            .collect();
        for path in &order {
            let l = layer[path];
            let idx = (l - 1) as usize;
            new_waves[idx]
                .files
                .push(files_by_path.get(path).unwrap().clone());
        }

        // Drop any empty waves (shouldn't happen with Kahn's layering, but
        // guard against it so the ordinals stay sequential).
        new_waves.retain(|w| !w.files.is_empty());
        for (i, w) in new_waves.iter_mut().enumerate() {
            w.wave = (i + 1) as u32;
        }

        self.waves = new_waves;
        Ok(true)
    }
}
