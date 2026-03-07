use super::PlanStep;
use clickweave_core::Position;
use uuid::Uuid;

/// Extract JSON from text that may be wrapped in markdown code fences.
pub(crate) fn extract_json(text: &str) -> &str {
    let trimmed = text.trim();
    for fence in ["```json", "```"] {
        if let Some(start) = trimmed.find(fence) {
            let after_fence = &trimmed[start + fence.len()..];
            if let Some(end) = after_fence.find("```") {
                return after_fence[..end].trim();
            }
        }
    }
    trimmed
}

/// Check if a step is rejected by feature flags. Returns Some(reason) if rejected.
pub(crate) fn step_rejected_reason(
    step: &PlanStep,
    allow_ai_transforms: bool,
    allow_agent_steps: bool,
) -> Option<&'static str> {
    if !allow_agent_steps && matches!(step, PlanStep::AiStep { .. }) {
        return Some("AiStep rejected (agent steps disabled)");
    }
    if !allow_ai_transforms && matches!(step, PlanStep::AiTransform { .. }) {
        return Some("AiTransform rejected (AI transforms disabled)");
    }
    None
}

/// Lay out nodes in a vertical chain.
pub(crate) fn layout_nodes(count: usize) -> Vec<Position> {
    (0..count)
        .map(|i| Position {
            x: 300.0,
            y: 100.0 + (i as f32) * 120.0,
        })
        .collect()
}

pub(crate) fn truncate_intent(intent: &str) -> String {
    if intent.len() <= 50 {
        return intent.to_string();
    }
    let end = intent.floor_char_boundary(47);
    format!("{}...", &intent[..end])
}

pub(crate) fn id_str_short(id: &Uuid) -> String {
    format!("{:.8}", id.as_hyphenated())
}
