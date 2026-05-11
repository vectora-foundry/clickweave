//! Skill retrieval scoring + cross-tier merge.
//!
//! Phase 3 replaces the Phase 2 placeholder (`1.0` on signature match)
//! with the rich formula: structural match + text similarity +
//! occurrence boost + success-rate weight, all multiplied by an
//! exponential time-decay term. The cross-tier merge prefers
//! project-local skills (1.3× multiplier) and caps global hits at one
//! per retrieval so a popular global skill cannot crowd out the local
//! tier.

#![allow(dead_code)]

use chrono::{DateTime, Utc};

use super::types::{RetrievedSkill, Skill, SkillScope, SkillState, SubgoalSignature};
use crate::agent::episodic::embedder::cosine;

/// Weights for the four additive score terms before the time-decay
/// multiplier. Values match the design doc's *Retrieval § Scoring and
/// ranking* table.
#[derive(Debug, Clone, Copy)]
pub struct ScoringWeights {
    pub w_struct: f32,
    pub w_text: f32,
    pub w_occur: f32,
    pub w_success: f32,
    pub halflife_days: f32,
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            w_struct: 0.5,
            w_text: 0.2,
            w_occur: 0.15,
            w_success: 0.15,
            halflife_days: 90.0,
        }
    }
}

/// Multiplier applied to project-local scores before the cross-tier
/// merge so an in-project skill outranks a global skill at equal raw
/// score.
pub const PROJECT_LOCAL_MULTIPLIER: f32 = 1.3;

/// Maximum number of global-tier hits the cross-tier merge allows in a
/// single retrieval.
pub const GLOBAL_CAP_PER_RETRIEVAL: usize = 1;

/// Bonus added to leaf-skill scores so they outrank a compound skill
/// covering the same applicability surface.
pub const LEAF_BONUS: f32 = 0.1;

/// Compute the per-skill score (pre-tier multiplier).
pub fn score(
    skill: &Skill,
    query_subgoal_sig: &SubgoalSignature,
    query_embedding: &[f32],
    skill_embedding: &[f32],
    weights: &ScoringWeights,
    now: DateTime<Utc>,
) -> f32 {
    let struct_match = if &skill.subgoal_signature == query_subgoal_sig {
        1.0
    } else {
        0.0
    };
    let text_sim = cosine(query_embedding, skill_embedding).max(0.0);
    let occurrence_boost = (1.0 + skill.stats.occurrence_count as f32).ln();
    let last_seen = skill
        .stats
        .last_seen_at
        .or(skill.stats.last_invoked_at)
        .unwrap_or(skill.created_at);
    let age_days = (now - last_seen).num_days() as f32;
    let halflife = weights.halflife_days.max(1.0);
    let decay = (-age_days.max(0.0) / halflife).exp();

    let raw = weights.w_struct * struct_match
        + weights.w_text * text_sim
        + weights.w_occur * occurrence_boost
        + weights.w_success * skill.stats.success_rate;

    let leaf_bonus = if is_leaf_skill(skill) {
        LEAF_BONUS
    } else {
        0.0
    };

    (raw + leaf_bonus) * decay
}

/// Apply the project-local multiplier (and the global cap) to a
/// pre-scored, signature-matched candidate list. Returns the top-`k`
/// rows by final score with project-local + ≤1 global slots.
pub fn merge_tiers(
    candidates: Vec<(SkillScope, f32, RetrievedSkill)>,
    k: usize,
) -> Vec<RetrievedSkill> {
    if k == 0 {
        return Vec::new();
    }
    let mut weighted: Vec<(SkillScope, f32, RetrievedSkill)> = candidates
        .into_iter()
        .map(|(scope, raw, mut hit)| {
            let final_score = if matches!(scope, SkillScope::ProjectLocal) {
                raw * PROJECT_LOCAL_MULTIPLIER
            } else {
                raw
            };
            hit.score = final_score;
            (scope, final_score, hit)
        })
        .collect();

    weighted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut out = Vec::with_capacity(k);
    let mut global_used = 0usize;
    for (scope, _final, hit) in weighted {
        if out.len() >= k {
            break;
        }
        if matches!(scope, SkillScope::Global) {
            if global_used >= GLOBAL_CAP_PER_RETRIEVAL {
                continue;
            }
            global_used += 1;
        }
        out.push(hit);
    }
    out
}

fn is_leaf_skill(skill: &Skill) -> bool {
    // The compound-skill variant was removed as part of the
    // skill-only-shell rewrite. Every remaining `ActionSketchStep`
    // variant (`ToolCall`, `Loop`) is a leaf operation, so any skill
    // with an action_sketch counts as a leaf skill here.
    let _ = skill;
    true
}

/// Drafts are intentionally excluded from retrieval — only `Confirmed`
/// and `Promoted` skills are eligible.
pub fn is_retrieval_eligible(skill: &Skill) -> bool {
    matches!(skill.state, SkillState::Confirmed | SkillState::Promoted)
}

#[cfg(test)]
mod tests {
    use super::super::types::{
        ApplicabilityHints, ApplicabilitySignature, OutcomePredicate, SkillScope, SkillState,
        SkillStats, SubgoalSignature,
    };
    use super::*;
    use chrono::{Duration, TimeZone, Utc};
    use std::sync::Arc;

    fn skill_at(
        id: &str,
        sig: &str,
        scope: SkillScope,
        occurrence: u32,
        success: f32,
        last_seen: Option<DateTime<Utc>>,
    ) -> Skill {
        Skill {
            id: id.into(),
            version: 1,
            state: SkillState::Confirmed,
            scope,
            name: id.into(),
            description: String::new(),
            tags: vec![],
            subgoal_text: format!("subgoal {id}"),
            subgoal_signature: SubgoalSignature(sig.into()),
            applicability: ApplicabilityHints {
                apps: vec![],
                hosts: vec![],
                signature: ApplicabilitySignature("a".into()),
            },
            parameter_schema: vec![],
            action_sketch: vec![],
            outputs: vec![],
            outcome_predicate: OutcomePredicate::SubgoalCompleted {
                post_state_world_model_signature: None,
            },
            provenance: vec![],
            stats: SkillStats {
                occurrence_count: occurrence,
                success_rate: success,
                last_seen_at: last_seen,
                last_invoked_at: None,
            },
            edited_by_user: false,
            created_at: Utc.timestamp_opt(0, 0).unwrap(),
            updated_at: Utc.timestamp_opt(0, 0).unwrap(),
            produced_node_ids: vec![],
            body: String::new(),
            schema_version: super::super::SKILL_SCHEMA_VERSION,
            variables: vec![],
            sections: vec![],
            replay: None,
        }
    }

    #[test]
    fn struct_match_contributes_w_struct() {
        let weights = ScoringWeights::default();
        let now = Utc::now();
        let s = skill_at("a", "sig", SkillScope::ProjectLocal, 1, 1.0, Some(now));
        let matched = score(&s, &SubgoalSignature("sig".into()), &[], &[], &weights, now);
        let unmatched = score(
            &s,
            &SubgoalSignature("other".into()),
            &[],
            &[],
            &weights,
            now,
        );
        assert!(matched > unmatched);
        assert!(
            (matched - unmatched - weights.w_struct).abs() < 1e-3,
            "structural component should contribute exactly w_struct"
        );
    }

    #[test]
    fn occurrence_count_increases_score_logarithmically() {
        let weights = ScoringWeights::default();
        let now = Utc::now();
        let low = skill_at("low", "sig", SkillScope::ProjectLocal, 1, 1.0, Some(now));
        let high = skill_at("hi", "sig", SkillScope::ProjectLocal, 100, 1.0, Some(now));
        let s_low = score(
            &low,
            &SubgoalSignature("sig".into()),
            &[],
            &[],
            &weights,
            now,
        );
        let s_high = score(
            &high,
            &SubgoalSignature("sig".into()),
            &[],
            &[],
            &weights,
            now,
        );
        assert!(s_high > s_low);
    }

    #[test]
    fn decay_reduces_score_for_older_skills() {
        let weights = ScoringWeights::default();
        let now = Utc::now();
        let young = skill_at("y", "sig", SkillScope::ProjectLocal, 5, 1.0, Some(now));
        let old = skill_at(
            "o",
            "sig",
            SkillScope::ProjectLocal,
            5,
            1.0,
            Some(now - Duration::days(180)),
        );
        let s_young = score(
            &young,
            &SubgoalSignature("sig".into()),
            &[],
            &[],
            &weights,
            now,
        );
        let s_old = score(
            &old,
            &SubgoalSignature("sig".into()),
            &[],
            &[],
            &weights,
            now,
        );
        assert!(s_young > s_old);
        assert!(
            s_old < s_young * 0.6,
            "180-day-old skill should score noticeably lower than fresh"
        );
    }

    #[test]
    fn project_local_multiplier_promotes_local_above_global_at_equal_raw_score() {
        let now = Utc::now();
        let local = skill_at("l", "sig", SkillScope::ProjectLocal, 1, 1.0, Some(now));
        let global = skill_at("g", "sig", SkillScope::Global, 1, 1.0, Some(now));
        let raw = 1.0;
        let merged = merge_tiers(
            vec![
                (
                    SkillScope::Global,
                    raw,
                    RetrievedSkill {
                        skill: Arc::new(global),
                        score: raw,
                    },
                ),
                (
                    SkillScope::ProjectLocal,
                    raw,
                    RetrievedSkill {
                        skill: Arc::new(local),
                        score: raw,
                    },
                ),
            ],
            5,
        );
        assert_eq!(
            merged.first().unwrap().skill.scope,
            SkillScope::ProjectLocal
        );
    }

    #[test]
    fn global_cap_caps_global_hits_at_one() {
        let now = Utc::now();
        let candidates = (0..5)
            .map(|i| {
                let s = skill_at(
                    &format!("g{i}"),
                    "sig",
                    SkillScope::Global,
                    1,
                    1.0,
                    Some(now),
                );
                (
                    SkillScope::Global,
                    1.0,
                    RetrievedSkill {
                        skill: Arc::new(s),
                        score: 1.0,
                    },
                )
            })
            .collect();
        let out = merge_tiers(candidates, 5);
        assert_eq!(out.len(), GLOBAL_CAP_PER_RETRIEVAL);
    }

    // The legacy "leaf vs compound" test relied on the compound-skill
    // variant that was removed as part of the skill-only-shell rewrite.
    // Every remaining variant is a leaf operation, so the leaf-bonus
    // signal collapses to a no-op and there is no meaningful pairing
    // left to assert here.
}
