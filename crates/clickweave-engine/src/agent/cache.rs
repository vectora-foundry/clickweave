use super::transition::page_fingerprint;
use super::types::{AgentCache, CachedDecision};
use clickweave_core::cdp::CdpFindElementMatch;

/// Generate a cache key from the goal and current page elements.
///
/// The key combines the goal text with a page fingerprint so that the same
/// page visited for different goals produces different cache keys. The
/// goal is trimmed and lowercased so trivial whitespace/casing differences
/// (`"Login"` vs `" login "`) still hit the same cache entry.
///
/// Crate-internal: the cache module is private (`mod cache;` in `mod.rs`)
/// so this helper is only reachable inside `clickweave-engine`.
pub(crate) fn cache_key(goal: &str, elements: &[CdpFindElementMatch]) -> String {
    let fp = page_fingerprint(elements);
    let normalized = goal.trim().to_lowercase();
    format!("{}|{}", normalized, fp)
}

impl AgentCache {
    /// Look up a cached decision for the given goal and elements.
    ///
    /// Returns the cached decision only if the element fingerprint still matches,
    /// ensuring stale cache entries are not reused when the page has changed.
    ///
    /// Crate-internal: called by `StateRunner` during exact-replay.
    pub(crate) fn lookup(
        &self,
        goal: &str,
        elements: &[CdpFindElementMatch],
    ) -> Option<&CachedDecision> {
        let key = cache_key(goal, elements);
        // The key already encodes the page fingerprint, so a hit implies fingerprint match.
        self.entries.get(&key)
    }

    /// Store a decision in the cache.
    ///
    /// Crate-internal: prefer `store_with_node` in new code. Retained for
    /// tests that do not need lineage tracking.
    pub(crate) fn store(
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
            produced_node_ids: Vec::new(),
        });
        entry.tool_name = tool_name;
        entry.arguments = arguments;
        entry.element_fingerprint = fp;
        entry.hit_count += 1;
    }

    /// Store a decision in the cache and record the node UUID it produced.
    /// Prefer this over `store` — it keeps cache-lineage Vec correct so
    /// `evict_for_node` can prune the right entries later.
    ///
    /// Crate-internal: called by `StateRunner` after a successful turn.
    pub(crate) fn store_with_node(
        &mut self,
        goal: &str,
        elements: &[CdpFindElementMatch],
        tool_name: String,
        arguments: serde_json::Value,
        produced_node_id: uuid::Uuid,
    ) {
        let key = cache_key(goal, elements);
        let fp = page_fingerprint(elements);
        let entry = self.entries.entry(key).or_insert_with(|| CachedDecision {
            tool_name: String::new(),
            arguments: serde_json::Value::Null,
            element_fingerprint: String::new(),
            hit_count: 0,
            produced_node_ids: Vec::new(),
        });
        entry.tool_name = tool_name;
        entry.arguments = arguments;
        entry.element_fingerprint = fp;
        entry.hit_count += 1;
        entry.produced_node_ids.push(produced_node_id);
    }

    /// Remove a node-id from any cache entry that tracks it. When an
    /// entry's `produced_node_ids` Vec becomes empty the whole entry is
    /// evicted. Called by Clear-conversation and Selective-delete.
    ///
    /// Cache entries deserialized from disk with an empty
    /// `produced_node_ids` are also dropped on first call: orphaned
    /// entries are harmless (Clear-conversation wipes the file anyway)
    /// but they never hit the lineage check, so eviction keeps the cache
    /// from accumulating stale rows.
    pub fn evict_for_node(&mut self, node_id: uuid::Uuid) {
        self.entries.retain(|_, entry| {
            entry.produced_node_ids.retain(|id| *id != node_id);
            !entry.produced_node_ids.is_empty()
        });
    }

    /// Remove a cached decision for the given goal and elements.
    ///
    /// Crate-internal: used by `StateRunner` when evicting a stale entry
    /// after a cache hit fails validation or a user rejects a cached
    /// action.
    pub(crate) fn remove(&mut self, goal: &str, elements: &[CdpFindElementMatch]) {
        let key = cache_key(goal, elements);
        self.entries.remove(&key);
    }

    /// Load cache entries from a JSON string.
    ///
    /// Crate-internal: external callers should use `load_from_path`.
    pub(crate) fn load(json: &str) -> Result<Self, serde_json::Error> {
        let entries = serde_json::from_str(json)?;
        Ok(Self { entries })
    }

    /// Serialize cache entries to a JSON string.
    ///
    /// Crate-internal: external callers should use `save_to_path`.
    pub(crate) fn save(&self) -> Result<String, serde_json::Error> {
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
        let json = self.save().map_err(std::io::Error::other)?;
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

    #[test]
    fn store_with_node_appends_produced_node_id() {
        let mut cache = AgentCache::default();
        let elements = make_elements();
        let node_id = uuid::Uuid::new_v4();

        cache.store_with_node(
            "login",
            &elements,
            "click".to_string(),
            serde_json::json!({"uid": "1_0"}),
            node_id,
        );

        let cached = cache.lookup("login", &elements).unwrap();
        assert_eq!(cached.produced_node_ids, vec![node_id]);
    }

    #[test]
    fn store_with_node_accumulates_multiple_nodes() {
        let mut cache = AgentCache::default();
        let elements = make_elements();
        let n1 = uuid::Uuid::new_v4();
        let n2 = uuid::Uuid::new_v4();

        cache.store_with_node("g", &elements, "click".into(), serde_json::json!({}), n1);
        cache.store_with_node("g", &elements, "click".into(), serde_json::json!({}), n2);

        let cached = cache.lookup("g", &elements).unwrap();
        assert_eq!(cached.produced_node_ids, vec![n1, n2]);
        assert_eq!(cached.hit_count, 2);
    }

    #[test]
    fn evict_for_node_removes_only_matching_entry() {
        let mut cache = AgentCache::default();
        let elements = make_elements();
        let n = uuid::Uuid::new_v4();
        cache.store_with_node("g", &elements, "click".into(), serde_json::json!({}), n);

        cache.evict_for_node(n);

        assert!(
            cache.lookup("g", &elements).is_none(),
            "entry whose only node was evicted must be removed"
        );
    }

    #[test]
    fn evict_for_node_keeps_entry_while_other_nodes_remain() {
        let mut cache = AgentCache::default();
        let elements = make_elements();
        let n1 = uuid::Uuid::new_v4();
        let n2 = uuid::Uuid::new_v4();
        cache.store_with_node("g", &elements, "click".into(), serde_json::json!({}), n1);
        cache.store_with_node("g", &elements, "click".into(), serde_json::json!({}), n2);

        cache.evict_for_node(n1);

        let cached = cache
            .lookup("g", &elements)
            .expect("entry should survive while n2 still references it");
        assert_eq!(cached.produced_node_ids, vec![n2]);
    }

    #[test]
    fn evict_for_node_is_noop_when_no_match() {
        let mut cache = AgentCache::default();
        let elements = make_elements();
        let stored = uuid::Uuid::new_v4();
        cache.store_with_node(
            "g",
            &elements,
            "click".into(),
            serde_json::json!({}),
            stored,
        );

        cache.evict_for_node(uuid::Uuid::new_v4()); // unrelated id

        assert!(cache.lookup("g", &elements).is_some());
    }
}
