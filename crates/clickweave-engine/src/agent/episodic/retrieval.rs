//! Pure scoring function. Takes a candidate episode and a query embedding,
//! produces a `ScoreBreakdown` per the formula in the design doc
//! ("Scoring formula" section). Cross-tier merge and workflow-priority
//! multiplication happen at the store call site, not here.

#![allow(dead_code)]

use chrono::{DateTime, Utc};

use crate::agent::episodic::embedder::cosine;
use crate::agent::episodic::types::{EpisodeRecord, ScoreBreakdown};

#[derive(Debug, Clone, Copy)]
pub struct ScoreWeights {
    pub structured: f32,
    pub text: f32,
    pub occurrence: f32,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            structured: 0.6,
            text: 0.3,
            occurrence: 0.1,
        }
    }
}

pub fn score(
    candidate: &EpisodeRecord,
    query_embedding: &[f32],
    now: DateTime<Utc>,
    weights: ScoreWeights,
    halflife_days: f32,
    structured_matched: bool,
) -> ScoreBreakdown {
    let text_similarity = if candidate.embedding_impl_id == "hashed_shingle_v1" {
        cosine(query_embedding, &candidate.goal_subgoal_embedding).max(0.0)
    } else {
        0.0 // D1.M4: impl-id mismatch — skip text scoring, keep structured
    };

    let occurrence_boost = ((candidate.occurrence_count as f32) + 1.0).ln();

    let age_days = (now - candidate.last_seen_at).num_seconds().max(0) as f32 / 86_400.0;
    let decay_factor = (-age_days / halflife_days.max(1.0)).exp();

    let structured_contrib = if structured_matched { 1.0 } else { 0.0 };
    let raw = weights.structured * structured_contrib
        + weights.text * text_similarity
        + weights.occurrence * occurrence_boost;

    let final_score = raw * decay_factor;
    let final_score = if final_score.is_finite() {
        final_score
    } else {
        0.0
    };

    ScoreBreakdown {
        structured_match: structured_matched,
        text_similarity,
        occurrence_boost,
        decay_factor,
        final_score,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::episodic::types::{
        CompactAction, EpisodeScope, FailureSignature, PreStateSignature, RecoveryActionsHash,
    };
    use crate::agent::step_record::WorldModelSnapshot;
    use chrono::Duration;

    /// `WorldModelSnapshot` is serialize-only and has no `Default` impl;
    /// build a fully-empty snapshot here so test rows don't pin Spec 1
    /// internals.
    fn empty_snapshot() -> WorldModelSnapshot {
        WorldModelSnapshot {
            focused_app: None,
            window_list: None,
            cdp_page: None,
            element_summary: None,
            modal_present: None,
            dialog_present: None,
            last_screenshot: None,
            last_native_ax_snapshot: None,
            uncertainty: Default::default(),
        }
    }

    fn mk_candidate(now: DateTime<Utc>, age_days: i64, count: u32) -> EpisodeRecord {
        EpisodeRecord {
            episode_id: "ep_test".into(),
            scope: EpisodeScope::WorkflowLocal,
            workflow_hash: "w-1".into(),
            pre_state_signature: PreStateSignature("abcd0123abcd0123".into()),
            goal: "login".into(),
            subgoal_text: Some("sign in".into()),
            failure_signature: FailureSignature {
                failed_tool: "cdp_click".into(),
                error_kind: "NotFound".into(),
                consecutive_errors_at_entry: 1,
            },
            recovery_actions: vec![CompactAction {
                tool_name: "ax_click".into(),
                brief_args: "button Continue".into(),
                outcome_kind: "ok".into(),
            }],
            recovery_actions_hash: RecoveryActionsHash("aaaa1111".into()),
            outcome_summary: "subgoal completed".into(),
            pre_state_snapshot: empty_snapshot(),
            goal_subgoal_embedding: vec![0.1; 4096],
            embedding_impl_id: "hashed_shingle_v1".into(),
            occurrence_count: count,
            created_at: now - Duration::days(age_days),
            last_seen_at: now - Duration::days(age_days),
            last_retrieved_at: None,
            step_record_refs: vec![],
        }
    }

    #[test]
    fn structured_match_dominates_score() {
        let now = Utc::now();
        let c = mk_candidate(now, 0, 1);
        let query = vec![0.0; 4096];
        let matched = score(&c, &query, now, ScoreWeights::default(), 90.0, true);
        let unmatched = score(&c, &query, now, ScoreWeights::default(), 90.0, false);
        assert!(matched.final_score > unmatched.final_score);
    }

    #[test]
    fn decay_factor_drops_with_age() {
        let now = Utc::now();
        let fresh = mk_candidate(now, 0, 1);
        let old = mk_candidate(now, 180, 1);
        let query = vec![0.0; 4096];
        let s_fresh = score(&fresh, &query, now, ScoreWeights::default(), 90.0, true);
        let s_old = score(&old, &query, now, ScoreWeights::default(), 90.0, true);
        assert!(s_fresh.final_score > s_old.final_score);
    }

    #[test]
    fn occurrence_count_boosts_score() {
        let now = Utc::now();
        let one = mk_candidate(now, 0, 1);
        let many = mk_candidate(now, 0, 10);
        let query = vec![0.0; 4096];
        let s_one = score(&one, &query, now, ScoreWeights::default(), 90.0, true);
        let s_many = score(&many, &query, now, ScoreWeights::default(), 90.0, true);
        assert!(s_many.final_score > s_one.final_score);
    }

    #[test]
    fn mismatched_impl_id_zeros_text_similarity() {
        let now = Utc::now();
        let mut c = mk_candidate(now, 0, 1);
        c.embedding_impl_id = "future_v2".into();
        let query = vec![0.1; 4096];
        let s = score(&c, &query, now, ScoreWeights::default(), 90.0, true);
        assert_eq!(s.text_similarity, 0.0);
    }

    #[test]
    fn final_score_never_nan() {
        let now = Utc::now();
        let c = mk_candidate(now, 0, 1);
        let query: Vec<f32> = std::iter::repeat_n(f32::NAN, 4096).collect();
        let s = score(&c, &query, now, ScoreWeights::default(), 90.0, true);
        assert!(s.final_score.is_finite());
    }
}
