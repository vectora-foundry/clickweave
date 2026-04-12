use super::transition::page_fingerprint;
use super::types::{AgentCache, CachedDecision};
use clickweave_core::cdp::CdpFindElementMatch;

/// Generate a cache key from the goal and current page elements.
///
/// The key combines the goal text with a page fingerprint so that the same
/// page visited for different goals produces different cache keys.
pub fn cache_key(goal: &str, elements: &[CdpFindElementMatch]) -> String {
    let fp = page_fingerprint(elements);
    format!("{}|{}", goal, fp)
}

impl AgentCache {
    /// Look up a cached decision for the given goal and elements.
    ///
    /// Returns the cached decision only if the element fingerprint still matches,
    /// ensuring stale cache entries are not reused when the page has changed.
    pub fn lookup(&self, goal: &str, elements: &[CdpFindElementMatch]) -> Option<&CachedDecision> {
        let key = cache_key(goal, elements);
        // The key already encodes the page fingerprint, so a hit implies fingerprint match.
        self.entries.get(&key)
    }

    /// Store a decision in the cache.
    pub fn store(
        &mut self,
        goal: &str,
        elements: &[CdpFindElementMatch],
        tool_name: String,
        arguments: serde_json::Value,
    ) {
        let key = cache_key(goal, elements);
        let fp = page_fingerprint(elements);
        let entry = self.entries.entry(key).or_insert_with(|| CachedDecision {
            tool_name: String::new(),
            arguments: serde_json::Value::Null,
            element_fingerprint: String::new(),
            hit_count: 0,
        });
        entry.tool_name = tool_name;
        entry.arguments = arguments;
        entry.element_fingerprint = fp;
        entry.hit_count += 1;
    }

    /// Remove a cached decision for the given goal and elements.
    pub fn remove(&mut self, goal: &str, elements: &[CdpFindElementMatch]) {
        let key = cache_key(goal, elements);
        self.entries.remove(&key);
    }

    /// Load cache entries from a JSON string.
    pub fn load(json: &str) -> Result<Self, serde_json::Error> {
        let entries = serde_json::from_str(json)?;
        Ok(Self { entries })
    }

    /// Serialize cache entries to a JSON string.
    pub fn save(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.entries)
    }

    /// Load cache from a file path. Returns an empty cache if the file
    /// doesn't exist or can't be parsed.
    pub fn load_from_path(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(json) => Self::load(&json).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save cache to a file path.
    pub fn save_to_path(&self, path: &std::path::Path) -> std::io::Result<()> {
        let json = self
            .save()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_elements() -> Vec<CdpFindElementMatch> {
        vec![
            CdpFindElementMatch {
                uid: "1_0".to_string(),
                role: "button".to_string(),
                label: "Submit".to_string(),
                tag: "button".to_string(),
                disabled: false,
                parent_role: None,
                parent_name: None,
            },
            CdpFindElementMatch {
                uid: "1_1".to_string(),
                role: "textbox".to_string(),
                label: "Email".to_string(),
                tag: "input".to_string(),
                disabled: false,
                parent_role: None,
                parent_name: None,
            },
        ]
    }

    #[test]
    fn cache_key_includes_goal_and_fingerprint() {
        let elements = make_elements();
        let key = cache_key("login", &elements);
        assert!(key.starts_with("login|"));
        assert!(key.len() > "login|".len());
    }

    #[test]
    fn cache_key_differs_for_different_goals() {
        let elements = make_elements();
        let key1 = cache_key("login", &elements);
        let key2 = cache_key("signup", &elements);
        assert_ne!(key1, key2);
    }

    #[test]
    fn store_and_lookup() {
        let mut cache = AgentCache::default();
        let elements = make_elements();

        cache.store(
            "login",
            &elements,
            "click".to_string(),
            serde_json::json!({"uid": "1_0"}),
        );

        let cached = cache.lookup("login", &elements);
        assert!(cached.is_some());
        let decision = cached.unwrap();
        assert_eq!(decision.tool_name, "click");
        assert_eq!(decision.hit_count, 1);
    }

    #[test]
    fn lookup_misses_for_different_elements() {
        let mut cache = AgentCache::default();
        let elements = make_elements();

        cache.store(
            "login",
            &elements,
            "click".to_string(),
            serde_json::json!({"uid": "1_0"}),
        );

        let different_elements = vec![CdpFindElementMatch {
            uid: "2_0".to_string(),
            role: "heading".to_string(),
            label: "Dashboard".to_string(),
            tag: "h1".to_string(),
            disabled: false,
            parent_role: None,
            parent_name: None,
        }];

        let cached = cache.lookup("login", &different_elements);
        assert!(cached.is_none());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let mut cache = AgentCache::default();
        let elements = make_elements();

        cache.store(
            "login",
            &elements,
            "click".to_string(),
            serde_json::json!({"uid": "1_0"}),
        );

        let json = cache.save().unwrap();
        let loaded = AgentCache::load(&json).unwrap();
        let cached = loaded.lookup("login", &elements);
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().tool_name, "click");
    }

    #[test]
    fn store_increments_hit_count() {
        let mut cache = AgentCache::default();
        let elements = make_elements();

        cache.store(
            "login",
            &elements,
            "click".to_string(),
            serde_json::json!({"uid": "1_0"}),
        );
        cache.store(
            "login",
            &elements,
            "click".to_string(),
            serde_json::json!({"uid": "1_0"}),
        );

        let cached = cache.lookup("login", &elements).unwrap();
        assert_eq!(cached.hit_count, 2);
    }
}
