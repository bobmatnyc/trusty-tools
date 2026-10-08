//! `gchat-spaces.json`: which DM space each route's recipient uses.
//!
//! Why: the app cannot open a DM with an external user; the recipient
//! messages the app first and the space is learned then (#9448 ruling 4).
//! What: [`SpaceBook`] maps a route name to `{recipient, space}`. A binding
//! counts only while its recipient still equals the route's recipient, so a
//! route edited to a new recipient never inherits the old DM.
//! Test: `bootstrap_binds_only_recipient_dm`,
//! `unlearned_space_is_refused_without_fallback`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::gchat::error::StateError;
use crate::gchat::state::{json_error_class, now_rfc3339, write_atomic};

/// One learned binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceBinding {
    /// The recipient email the space was learned from.
    pub recipient: String,
    /// The DM space, `spaces/{space}`.
    pub space: String,
    /// When it was learned (RFC 3339).
    pub learned_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SpacesFile {
    version: u32,
    #[serde(default)]
    spaces: BTreeMap<String, SpaceBinding>,
}

/// The learned route → space map, persisted on every change.
#[derive(Debug)]
pub struct SpaceBook {
    path: PathBuf,
    spaces: BTreeMap<String, SpaceBinding>,
}

impl SpaceBook {
    /// Read `path`, or start empty when it does not exist.
    pub fn open(path: &Path) -> Result<Self, StateError> {
        let spaces = match std::fs::read(path) {
            Ok(bytes) => {
                let file: SpacesFile =
                    serde_json::from_slice(&bytes).map_err(|e| StateError::Corrupt {
                        path: path.to_path_buf(),
                        line: 0,
                        reason: json_error_class(&e).to_string(),
                    })?;
                file.spaces
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(StateError::io(path, &e)),
        };
        Ok(Self {
            path: path.to_path_buf(),
            spaces,
        })
    }

    /// The space bound to `route`, if it was learned from `recipient`.
    pub fn space_for(&self, route: &str, recipient: &str) -> Option<&str> {
        self.spaces
            .get(route)
            .filter(|b| b.recipient == recipient)
            .map(|b| b.space.as_str())
    }

    /// Bind `route` to `space` and persist. Returns false (and writes
    /// nothing) when the route already holds a binding for `recipient`.
    pub fn learn(&mut self, route: &str, recipient: &str, space: &str) -> Result<bool, StateError> {
        if self.space_for(route, recipient).is_some() {
            return Ok(false);
        }
        let mut next = self.spaces.clone();
        next.insert(
            route.to_string(),
            SpaceBinding {
                recipient: recipient.to_string(),
                space: space.to_string(),
                learned_at: now_rfc3339(),
            },
        );
        let file = SpacesFile {
            version: 1,
            spaces: next,
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|e| StateError::Corrupt {
            path: self.path.clone(),
            line: 0,
            reason: format!("cannot encode: {e}"),
        })?;
        write_atomic(&self.path, &bytes)?;
        self.spaces = file.spaces;
        Ok(true)
    }
}
