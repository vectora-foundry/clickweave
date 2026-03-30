use super::helpers::*;
use clickweave_core::AppKind;
use clickweave_core::{
    ClickParams, ClickTarget, FindTextParams, FocusMethod, FocusWindowParams, McpToolCallParams,
    NodeType, ScreenshotMode, TakeScreenshotParams, TypeTextParams,
};

use super::super::ResolvedApp;

// ---------------------------------------------------------------------------
// App cache tests
// ---------------------------------------------------------------------------

#[test]
fn evict_app_cache_removes_entry() {
    let exec = make_test_executor();

    // Insert a resolved app into the cache
    exec.app_cache.write().unwrap().insert(
        "chrome".to_string(),
        ResolvedApp {
            name: "Google Chrome".to_string(),
            pid: 1234,
        },
    );
    assert!(exec.app_cache.read().unwrap().contains_key("chrome"));

    // Evict it
    exec.evict_app_cache("chrome");
    assert!(
        !exec.app_cache.read().unwrap().contains_key("chrome"),
        "cache entry should be removed after eviction"
    );
}

#[test]
fn evict_app_cache_noop_for_missing_key() {
    let exec = make_test_executor();

    // Evicting a key that was never cached should not panic
    exec.evict_app_cache("nonexistent");
    assert!(exec.app_cache.read().unwrap().is_empty());
}

#[test]
fn evict_app_cache_leaves_other_entries() {
    let exec = make_test_executor();

    exec.app_cache.write().unwrap().insert(
        "chrome".to_string(),
        ResolvedApp {
            name: "Google Chrome".to_string(),
            pid: 1234,
        },
    );
    exec.app_cache.write().unwrap().insert(
        "firefox".to_string(),
        ResolvedApp {
            name: "Firefox".to_string(),
            pid: 5678,
        },
    );

    exec.evict_app_cache("chrome");

    assert!(
        !exec.app_cache.read().unwrap().contains_key("chrome"),
        "evicted entry should be gone"
    );
    assert!(
        exec.app_cache.read().unwrap().contains_key("firefox"),
        "other entries should remain"
    );
}

#[test]
fn evict_app_cache_for_focus_window_node() {
    let exec = make_test_executor();
    exec.app_cache.write().unwrap().insert(
        "chrome".to_string(),
        ResolvedApp {
            name: "Google Chrome".to_string(),
            pid: 1234,
        },
    );

    let node = NodeType::FocusWindow(FocusWindowParams {
        method: FocusMethod::AppName,
        value: Some("chrome".to_string()),
        bring_to_front: true,
        app_kind: clickweave_core::AppKind::Native,
        chrome_profile_id: None,
        ..Default::default()
    });
    exec.evict_caches_for_node(&node);
    assert!(!exec.app_cache.read().unwrap().contains_key("chrome"));
}

#[test]
fn evict_app_cache_for_screenshot_node() {
    let exec = make_test_executor();
    exec.app_cache.write().unwrap().insert(
        "safari".to_string(),
        ResolvedApp {
            name: "Safari".to_string(),
            pid: 999,
        },
    );

    let node = NodeType::TakeScreenshot(TakeScreenshotParams {
        mode: ScreenshotMode::Window,
        target: Some("safari".to_string()),
        include_ocr: true,
    });
    exec.evict_caches_for_node(&node);
    assert!(!exec.app_cache.read().unwrap().contains_key("safari"));
}

#[test]
fn evict_app_cache_for_unrelated_node_is_noop() {
    let exec = make_test_executor();
    exec.app_cache.write().unwrap().insert(
        "chrome".to_string(),
        ResolvedApp {
            name: "Google Chrome".to_string(),
            pid: 1234,
        },
    );

    let node = NodeType::Click(clickweave_core::ClickParams::default());
    exec.evict_caches_for_node(&node);
    assert!(exec.app_cache.read().unwrap().contains_key("chrome"));
}

// ---------------------------------------------------------------------------
// Element cache eviction tests
// ---------------------------------------------------------------------------

#[test]
fn evict_element_cache_for_click_node() {
    let exec = make_test_executor();
    let cache_key = ("×".to_string(), Some("Calculator".to_string()));
    exec.element_cache
        .write()
        .unwrap()
        .insert(cache_key.clone(), "Multiply".to_string());

    // Set focused_app so eviction uses the right cache key
    *exec.focused_app.write().unwrap() = Some(("Calculator".to_string(), AppKind::Native, 0));

    let node = NodeType::Click(ClickParams {
        target: Some(ClickTarget::Text {
            text: "×".to_string(),
        }),
        ..ClickParams::default()
    });
    exec.evict_caches_for_node(&node);
    assert!(
        !exec.element_cache.read().unwrap().contains_key(&cache_key),
        "element cache entry should be evicted for Click node"
    );
}

#[test]
fn evict_element_cache_for_find_text_node() {
    let exec = make_test_executor();
    let cache_key = ("÷".to_string(), None);
    exec.element_cache
        .write()
        .unwrap()
        .insert(cache_key.clone(), "Divide".to_string());

    let node = NodeType::FindText(FindTextParams {
        search_text: "÷".to_string(),
        ..FindTextParams::default()
    });
    exec.evict_caches_for_node(&node);
    assert!(
        !exec.element_cache.read().unwrap().contains_key(&cache_key),
        "element cache entry should be evicted for FindText node"
    );
}

#[test]
fn evict_element_cache_noop_for_unrelated_node() {
    let exec = make_test_executor();
    let cache_key = ("×".to_string(), Some("Calculator".to_string()));
    exec.element_cache
        .write()
        .unwrap()
        .insert(cache_key.clone(), "Multiply".to_string());

    let node = NodeType::TypeText(TypeTextParams {
        text: "hello".to_string(),
        ..Default::default()
    });
    exec.evict_caches_for_node(&node);
    assert!(
        exec.element_cache.read().unwrap().contains_key(&cache_key),
        "element cache should not be evicted for unrelated node type"
    );
}

#[test]
fn evict_element_cache_for_mcp_find_text_node() {
    let exec = make_test_executor();
    let cache_key = ("×".to_string(), Some("Calculator".to_string()));
    exec.element_cache
        .write()
        .unwrap()
        .insert(cache_key.clone(), "Multiply".to_string());

    *exec.focused_app.write().unwrap() = Some(("Calculator".to_string(), AppKind::Native, 0));

    let node = NodeType::McpToolCall(McpToolCallParams {
        tool_name: "find_text".to_string(),
        arguments: serde_json::json!({"text": "×"}),
    });
    exec.evict_caches_for_node(&node);
    assert!(
        !exec.element_cache.read().unwrap().contains_key(&cache_key),
        "element cache entry should be evicted for McpToolCall(find_text) node"
    );
}

#[test]
fn evict_element_cache_for_mcp_find_text_with_explicit_app_name() {
    let exec = make_test_executor();
    // Cache keyed to explicit app_name "Safari", not focused_app "Calculator"
    let cache_key = ("link".to_string(), Some("Safari".to_string()));
    exec.element_cache
        .write()
        .unwrap()
        .insert(cache_key.clone(), "AXLink".to_string());

    *exec.focused_app.write().unwrap() = Some(("Calculator".to_string(), AppKind::Native, 0));

    let node = NodeType::McpToolCall(McpToolCallParams {
        tool_name: "find_text".to_string(),
        arguments: serde_json::json!({"text": "link", "app_name": "Safari"}),
    });
    exec.evict_caches_for_node(&node);
    assert!(
        !exec.element_cache.read().unwrap().contains_key(&cache_key),
        "element cache should use explicit app_name from arguments, not focused_app"
    );
}
