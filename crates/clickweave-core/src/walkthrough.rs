use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::MouseButton;
use crate::storage::{append_jsonl, format_timestamped_dirname, now_millis, write_json_pretty};

// --- Session ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct WalkthroughSession {
    pub id: Uuid,
    pub workflow_id: Uuid,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub status: WalkthroughStatus,
    #[serde(skip)]
    pub events: Vec<WalkthroughEvent>,
    #[serde(skip)]
    pub actions: Vec<WalkthroughAction>,
    pub warnings: Vec<String>,
}

impl WalkthroughSession {
    pub fn new(workflow_id: Uuid) -> Self {
        Self {
            id: Uuid::new_v4(),
            workflow_id,
            started_at: now_millis(),
            ended_at: None,
            status: WalkthroughStatus::Recording,
            events: Vec::new(),
            actions: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum WalkthroughStatus {
    #[default]
    Idle,
    Recording,
    Paused,
    Processing,
    Review,
    Applied,
    Cancelled,
}

// --- Raw capture events ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct OcrAnnotation {
    pub text: String,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct WalkthroughEvent {
    pub id: Uuid,
    pub timestamp: u64,
    pub kind: WalkthroughEventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum WalkthroughEventKind {
    AppFocused {
        app_name: String,
        pid: i32,
        window_title: Option<String>,
    },
    MouseClicked {
        x: f64,
        y: f64,
        button: MouseButton,
        click_count: u32,
        modifiers: Vec<String>,
    },
    KeyPressed {
        key: String,
        modifiers: Vec<String>,
    },
    TextCommitted {
        text: String,
    },
    Scrolled {
        delta_y: f64,
        x: Option<f64>,
        y: Option<f64>,
    },
    ScreenshotCaptured {
        path: String,
        kind: ScreenshotKind,
    },
    OcrCaptured {
        annotations: Vec<OcrAnnotation>,
        click_x: f64,
        click_y: f64,
    },
    Paused,
    Resumed,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum ScreenshotKind {
    BeforeClick,
    AfterClick,
    ClickCrop,
}

// --- Normalized semantic actions ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct WalkthroughAction {
    pub id: Uuid,
    pub kind: WalkthroughActionKind,
    pub app_name: Option<String>,
    pub window_title: Option<String>,
    pub target_candidates: Vec<TargetCandidate>,
    pub artifact_paths: Vec<String>,
    pub source_event_ids: Vec<Uuid>,
    pub confidence: ActionConfidence,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum WalkthroughActionKind {
    LaunchApp {
        app_name: String,
    },
    FocusWindow {
        app_name: String,
        window_title: Option<String>,
    },
    Click {
        x: f64,
        y: f64,
        button: MouseButton,
        click_count: u32,
    },
    TypeText {
        text: String,
    },
    PressKey {
        key: String,
        modifiers: Vec<String>,
    },
    Scroll {
        delta_y: f64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum TargetCandidate {
    AccessibilityLabel { label: String, role: Option<String> },
    OcrText { text: String },
    ImageCrop { path: String },
    Coordinates { x: f64, y: f64 },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum ActionConfidence {
    High,
    #[default]
    Medium,
    Low,
}

// --- Review annotations ---

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct WalkthroughAnnotations {
    pub deleted_action_ids: Vec<Uuid>,
    pub renamed_actions: Vec<ActionRename>,
    pub target_overrides: Vec<TargetOverride>,
    pub variable_promotions: Vec<VariablePromotion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ActionRename {
    pub action_id: Uuid,
    pub new_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TargetOverride {
    pub action_id: Uuid,
    pub chosen_candidate_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct VariablePromotion {
    pub action_id: Uuid,
    pub variable_name: String,
}

// --- Walkthrough storage ---

/// Manages on-disk storage for walkthrough session data and artifacts.
///
/// Directory layout:
/// ```text
/// walkthroughs/<timestamp>_<shortid>/
///   session.json
///   events.jsonl
///   actions.json
///   draft.json
///   artifacts/
/// ```
#[derive(Clone)]
pub struct WalkthroughStorage {
    base_path: std::path::PathBuf,
}

impl WalkthroughStorage {
    /// Create storage for a saved project.
    ///
    /// Path: `<project>/.clickweave/walkthroughs/`
    pub fn new(project_path: &std::path::Path) -> Self {
        Self {
            base_path: project_path.join(".clickweave").join("walkthroughs"),
        }
    }

    /// Create storage for an unsaved project (app data fallback).
    ///
    /// Path: `<app_data>/walkthroughs/`
    pub fn new_app_data(app_data_dir: &std::path::Path) -> Self {
        Self {
            base_path: app_data_dir.join("walkthroughs"),
        }
    }

    /// Create a directory for a new walkthrough session.
    /// Returns the full path to the session directory.
    pub fn create_session_dir(
        &self,
        session: &WalkthroughSession,
    ) -> anyhow::Result<std::path::PathBuf> {
        let dirname = format_timestamped_dirname(session.started_at, session.id);
        let session_dir = self.base_path.join(&dirname);
        std::fs::create_dir_all(session_dir.join("artifacts"))
            .map_err(|e| anyhow::anyhow!("Failed to create walkthrough session directory: {e}"))?;

        Ok(session_dir)
    }

    /// Save the session metadata to `session.json`.
    ///
    /// Note: `events` and `actions` are skipped during serialization —
    /// they live in `events.jsonl` and `actions.json` respectively.
    pub fn save_session(
        &self,
        session_dir: &std::path::Path,
        session: &WalkthroughSession,
    ) -> anyhow::Result<()> {
        write_json_pretty(&session_dir.join("session.json"), session)
    }

    /// Append a raw event to `events.jsonl`.
    pub fn append_event(
        &self,
        session_dir: &std::path::Path,
        event: &WalkthroughEvent,
    ) -> anyhow::Result<()> {
        append_jsonl(&session_dir.join("events.jsonl"), event)
    }

    /// Save the normalized actions to `actions.json`.
    pub fn save_actions(
        &self,
        session_dir: &std::path::Path,
        actions: &[WalkthroughAction],
    ) -> anyhow::Result<()> {
        write_json_pretty(&session_dir.join("actions.json"), actions)
    }

    /// Save a workflow draft to `draft.json`.
    pub fn save_draft(
        &self,
        session_dir: &std::path::Path,
        draft: &crate::Workflow,
    ) -> anyhow::Result<()> {
        write_json_pretty(&session_dir.join("draft.json"), draft)
    }

    pub fn base_path(&self) -> &std::path::Path {
        &self.base_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_walkthrough_status_default_is_idle() {
        assert_eq!(WalkthroughStatus::default(), WalkthroughStatus::Idle);
    }

    #[test]
    fn test_session_serialization_skips_events_and_actions() {
        let session = WalkthroughSession {
            id: Uuid::new_v4(),
            workflow_id: Uuid::new_v4(),
            started_at: 1_700_000_000_000,
            ended_at: None,
            status: WalkthroughStatus::Recording,
            events: vec![WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 1_700_000_000_100,
                kind: WalkthroughEventKind::Paused,
            }],
            actions: vec![],
            warnings: vec![],
        };

        let json = serde_json::to_string(&session).expect("serialize");
        assert!(!json.contains("Paused"));

        let deserialized: WalkthroughSession = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.id, session.id);
        assert_eq!(deserialized.status, WalkthroughStatus::Recording);
        assert!(deserialized.events.is_empty());
        assert!(deserialized.actions.is_empty());
    }

    #[test]
    fn test_walkthrough_status_serde_produces_expected_strings() {
        let variants = vec![
            (WalkthroughStatus::Idle, "\"Idle\""),
            (WalkthroughStatus::Recording, "\"Recording\""),
            (WalkthroughStatus::Paused, "\"Paused\""),
            (WalkthroughStatus::Processing, "\"Processing\""),
            (WalkthroughStatus::Review, "\"Review\""),
            (WalkthroughStatus::Applied, "\"Applied\""),
            (WalkthroughStatus::Cancelled, "\"Cancelled\""),
        ];
        for (variant, expected) in &variants {
            let json = serde_json::to_string(variant).expect("serialize");
            assert_eq!(
                &json, expected,
                "WalkthroughStatus::{variant:?} serialized incorrectly"
            );
        }
    }

    #[test]
    fn test_event_kind_serialization_roundtrip() {
        let kinds = vec![
            WalkthroughEventKind::AppFocused {
                app_name: "Calculator".to_string(),
                pid: 1234,
                window_title: Some("Calculator".to_string()),
            },
            WalkthroughEventKind::MouseClicked {
                x: 100.0,
                y: 200.0,
                button: MouseButton::Left,
                click_count: 1,
                modifiers: vec![],
            },
            WalkthroughEventKind::KeyPressed {
                key: "Enter".to_string(),
                modifiers: vec![],
            },
            WalkthroughEventKind::TextCommitted {
                text: "hello".to_string(),
            },
            WalkthroughEventKind::Scrolled {
                delta_y: -3.0,
                x: None,
                y: None,
            },
            WalkthroughEventKind::ScreenshotCaptured {
                path: "/tmp/shot.png".to_string(),
                kind: ScreenshotKind::BeforeClick,
            },
            WalkthroughEventKind::Paused,
            WalkthroughEventKind::Resumed,
            WalkthroughEventKind::Stopped,
        ];

        for kind in &kinds {
            let event = WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 1_700_000_000_000,
                kind: kind.clone(),
            };
            let json = serde_json::to_string(&event).expect("serialize");
            let deserialized: WalkthroughEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(deserialized.id, event.id);
        }
    }

    #[test]
    fn test_action_kind_serialization_roundtrip() {
        let kinds = vec![
            WalkthroughActionKind::LaunchApp {
                app_name: "Calculator".to_string(),
            },
            WalkthroughActionKind::FocusWindow {
                app_name: "Calculator".to_string(),
                window_title: Some("Calculator".to_string()),
            },
            WalkthroughActionKind::Click {
                x: 100.0,
                y: 200.0,
                button: MouseButton::Left,
                click_count: 1,
            },
            WalkthroughActionKind::TypeText {
                text: "hello".to_string(),
            },
            WalkthroughActionKind::PressKey {
                key: "Enter".to_string(),
                modifiers: vec![],
            },
            WalkthroughActionKind::Scroll { delta_y: -3.0 },
        ];

        for kind in &kinds {
            let json = serde_json::to_string(kind).expect("serialize");
            let _deserialized: WalkthroughActionKind =
                serde_json::from_str(&json).expect("deserialize");
        }
    }

    #[test]
    fn test_target_candidate_serialization_roundtrip() {
        let candidates = vec![
            TargetCandidate::AccessibilityLabel {
                label: "Submit".to_string(),
                role: Some("AXButton".to_string()),
            },
            TargetCandidate::OcrText {
                text: "Submit".to_string(),
            },
            TargetCandidate::ImageCrop {
                path: "/tmp/crop.png".to_string(),
            },
            TargetCandidate::Coordinates { x: 100.0, y: 200.0 },
        ];

        for candidate in &candidates {
            let json = serde_json::to_string(candidate).expect("serialize");
            let _deserialized: TargetCandidate = serde_json::from_str(&json).expect("deserialize");
        }
    }

    #[test]
    fn test_annotations_default() {
        let annotations = WalkthroughAnnotations::default();
        assert!(annotations.deleted_action_ids.is_empty());
        assert!(annotations.renamed_actions.is_empty());
        assert!(annotations.target_overrides.is_empty());
        assert!(annotations.variable_promotions.is_empty());
    }

    #[test]
    fn test_storage_create_and_save_session() {
        let dir = std::env::temp_dir()
            .join("clickweave_test_wt")
            .join(Uuid::new_v4().to_string());
        let storage = WalkthroughStorage::new(&dir);

        let session = WalkthroughSession {
            id: Uuid::new_v4(),
            workflow_id: Uuid::new_v4(),
            started_at: 1_700_000_000_000,
            ended_at: None,
            status: WalkthroughStatus::Recording,
            events: vec![],
            actions: vec![],
            warnings: vec![],
        };

        let session_dir = storage
            .create_session_dir(&session)
            .expect("create session dir");
        assert!(session_dir.exists());
        assert!(session_dir.join("artifacts").exists());

        storage
            .save_session(&session_dir, &session)
            .expect("save session");
        assert!(session_dir.join("session.json").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_storage_append_event() {
        let dir = std::env::temp_dir()
            .join("clickweave_test_wt")
            .join(Uuid::new_v4().to_string());
        let storage = WalkthroughStorage::new(&dir);

        let session = WalkthroughSession {
            id: Uuid::new_v4(),
            workflow_id: Uuid::new_v4(),
            started_at: 1_700_000_000_000,
            ended_at: None,
            status: WalkthroughStatus::Recording,
            events: vec![],
            actions: vec![],
            warnings: vec![],
        };

        let session_dir = storage
            .create_session_dir(&session)
            .expect("create session dir");

        let event = WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp: 1_700_000_000_100,
            kind: WalkthroughEventKind::AppFocused {
                app_name: "Calculator".to_string(),
                pid: 1234,
                window_title: Some("Calculator".to_string()),
            },
        };

        storage
            .append_event(&session_dir, &event)
            .expect("append event");

        let content =
            std::fs::read_to_string(session_dir.join("events.jsonl")).expect("read events.jsonl");
        assert!(content.contains("Calculator"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
