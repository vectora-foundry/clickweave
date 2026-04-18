use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use uuid::Uuid;

/// Caches LLM decisions made during Test mode so they can be replayed in Run mode
/// without repeating the LLM calls.
///
/// Stored as `decisions.json` alongside the workflow's run directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DecisionCache {
    pub version: u32,
    pub workflow_id: Uuid,
    /// Keyed by `"node_id\0target\0app_name"` (NUL separator cannot appear in UI text).
    pub click_disambiguation: HashMap<String, ClickDisambiguation>,
    /// Keyed by `"node_id\0target\0app_name"`.
    pub element_resolution: HashMap<String, ElementResolution>,
    /// Keyed by `"node_id\0user_input"`. Stores the resolved app name (not PID,
    /// since PIDs change between runs).
    #[serde(default)]
    pub app_resolution: HashMap<String, AppResolution>,
    #[serde(default)]
    pub cdp_port: HashMap<String, CdpPort>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickDisambiguation {
    pub target: String,
    pub app_name: Option<String>,
    pub chosen_text: String,
    pub chosen_role: String,
    /// Screen coordinates of the chosen element at the time the disambiguation
    /// was recorded. Used as a tiebreaker when multiple matches share the same
    /// text and role on replay.
    #[serde(default)]
    pub chosen_x: Option<f64>,
    #[serde(default)]
    pub chosen_y: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementResolution {
    pub target: String,
    pub resolved_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppResolution {
    pub user_input: String,
    pub resolved_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CdpPort {
    pub port: u16,
}

/// Build a cache key from a node ID, target, and optional app name.
/// Uses NUL as separator since it cannot appear in UI element text.
/// Node-scoped to prevent cross-node collisions for the same target.
pub fn cache_key(node_id: Uuid, target: &str, app_name: Option<&str>) -> String {
    match app_name {
        Some(app) => format!("{}\0{}\0{}", node_id, target, app),
        None => format!("{}\0{}", node_id, target),
    }
}

impl DecisionCache {
    pub fn new(workflow_id: Uuid) -> Self {
        Self {
            version: 1,
            workflow_id,
            ..Default::default()
        }
    }

    /// Load a decision cache from disk, validating that it belongs to the
    /// expected workflow. Returns `None` if the file does not exist, cannot
    /// be deserialized, or belongs to a different workflow.
    pub fn load(path: &Path, expected_workflow_id: Uuid) -> Option<Self> {
        let data = std::fs::read_to_string(path).ok()?;
        let cache: Self = serde_json::from_str(&data).ok()?;
        if cache.workflow_id != expected_workflow_id {
            return None;
        }
        Some(cache)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        crate::storage::write_json_atomic(path, self)
            .map_err(|e| format!("Failed to write cache: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_with_app() {
        let id = Uuid::nil();
        assert_eq!(
            cache_key(id, "2", Some("Calculator")),
            format!("{}\0{}\0{}", id, "2", "Calculator")
        );
    }

    #[test]
    fn cache_key_without_app() {
        let id = Uuid::nil();
        assert_eq!(
            cache_key(id, "Submit", None),
            format!("{}\0{}", id, "Submit")
        );
    }

    #[test]
    fn round_trip_save_load() {
        let dir = std::env::temp_dir()
            .join("clickweave_test_cache")
            .join(Uuid::new_v4().to_string());
        let path = dir.join("decisions.json");

        let node_id = Uuid::new_v4();
        let workflow_id = Uuid::new_v4();
        let mut cache = DecisionCache::new(workflow_id);
        cache.click_disambiguation.insert(
            cache_key(node_id, "2", Some("Calculator")),
            ClickDisambiguation {
                target: "2".to_string(),
                app_name: Some("Calculator".to_string()),
                chosen_text: "2".to_string(),
                chosen_role: "AXButton".to_string(),
                chosen_x: Some(100.0),
                chosen_y: Some(200.0),
            },
        );
        cache.element_resolution.insert(
            cache_key(node_id, "×", Some("Calculator")),
            ElementResolution {
                target: "×".to_string(),
                resolved_name: "Multiply".to_string(),
            },
        );

        cache.save(&path).expect("save");
        let loaded = DecisionCache::load(&path, workflow_id).expect("load");

        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.click_disambiguation.len(), 1);
        assert_eq!(loaded.element_resolution.len(), 1);

        let disambig = loaded
            .click_disambiguation
            .get(&cache_key(node_id, "2", Some("Calculator")))
            .unwrap();
        assert_eq!(disambig.chosen_text, "2");
        assert_eq!(disambig.chosen_role, "AXButton");
        assert_eq!(disambig.chosen_x, Some(100.0));
        assert_eq!(disambig.chosen_y, Some(200.0));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trip_cdp_port() {
        let dir = std::env::temp_dir()
            .join("clickweave_test_cache")
            .join(Uuid::new_v4().to_string());
        let path = dir.join("decisions.json");

        let workflow_id = Uuid::new_v4();
        let mut cache = DecisionCache::new(workflow_id);
        let key = "Discord".to_string();
        cache.cdp_port.insert(key.clone(), CdpPort { port: 52341 });

        cache.save(&path).expect("save");
        let loaded = DecisionCache::load(&path, workflow_id).expect("load");

        assert_eq!(loaded.cdp_port.len(), 1);
        let entry = loaded.cdp_port.get("Discord").unwrap();
        assert_eq!(entry.port, 52341);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_nonexistent_returns_none() {
        assert!(
            DecisionCache::load(std::path::Path::new("/nonexistent/path.json"), Uuid::nil())
                .is_none()
        );
    }

    #[test]
    fn load_rejects_wrong_workflow_id() {
        let dir = std::env::temp_dir()
            .join("clickweave_test_cache")
            .join(Uuid::new_v4().to_string());
        let path = dir.join("decisions.json");

        let workflow_id = Uuid::new_v4();
        let cache = DecisionCache::new(workflow_id);
        cache.save(&path).expect("save");

        // Load with the correct ID succeeds
        assert!(DecisionCache::load(&path, workflow_id).is_some());

        // Load with a different ID returns None
        let other_id = Uuid::new_v4();
        assert!(DecisionCache::load(&path, other_id).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
