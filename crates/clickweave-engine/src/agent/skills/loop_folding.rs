//! Repeating-sub-sequence detection that folds polling loops in a
//! recorded action trace into a single `ActionSketchStep::Loop` step.
//!
//! Run as the final pass of `provenance::build_action_sketch` so that
//! the captured action sketch keeps the LLM-facing sketch terse without
//! losing the polling-loop semantics.

#![allow(dead_code)]

use super::types::{ActionSketchStep, LoopPredicate};

const DEFAULT_ITERATION_DELAY_MS: u64 = 500;
const SAFETY_MARGIN_ITERATIONS: u32 = 3;

pub fn fold_polling_loops(steps: &mut Vec<ActionSketchStep>) {
    let mut i = 0;
    while i < steps.len() {
        if let Some((repeat_len, repeat_count)) = detect_repeat_starting_at(steps, i)
            && repeat_count >= 2
        {
            let body: Vec<ActionSketchStep> = steps.drain(i..i + repeat_len).collect();
            steps.drain(i..i + repeat_len * (repeat_count - 1));
            let until = synthesize_until_predicate(&body);
            let new_loop = ActionSketchStep::Loop {
                step_id: format!("s_loop_{:06}", i),
                until,
                body,
                max_iterations: repeat_count as u32 + SAFETY_MARGIN_ITERATIONS,
                iteration_delay_ms: DEFAULT_ITERATION_DELAY_MS,
            };
            steps.insert(i, new_loop);
        }
        i += 1;
    }
}

fn detect_repeat_starting_at(steps: &[ActionSketchStep], start: usize) -> Option<(usize, usize)> {
    let remaining = steps.len() - start;
    for repeat_len in 1..=(remaining / 2) {
        let mut count = 1;
        let mut cursor = start + repeat_len;
        while cursor + repeat_len <= steps.len()
            && steps_match_iter(
                &steps[start..start + repeat_len],
                &steps[cursor..cursor + repeat_len],
            )
        {
            count += 1;
            cursor += repeat_len;
        }
        if count >= 2 {
            return Some((repeat_len, count));
        }
    }
    None
}

fn steps_match_iter(a: &[ActionSketchStep], b: &[ActionSketchStep]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| step_shape_eq(x, y))
}

fn step_shape_eq(a: &ActionSketchStep, b: &ActionSketchStep) -> bool {
    match (a, b) {
        (
            ActionSketchStep::ToolCall {
                tool: t1, args: a1, ..
            },
            ActionSketchStep::ToolCall {
                tool: t2, args: a2, ..
            },
        ) => t1 == t2 && a1 == a2,
        _ => false,
    }
}

fn synthesize_until_predicate(_body: &[ActionSketchStep]) -> LoopPredicate {
    // Phase 1 lands a placeholder predicate that matches "any world-model
    // change". Phase 3's extractor refines this against the observed
    // pre/post deltas of the folded body.
    LoopPredicate::WorldModelDelta {
        expr: "world_model.changed".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::ExpectedWorldModelDelta;
    use super::*;
    use serde_json::json;

    fn tc(tool: &str, args: serde_json::Value) -> ActionSketchStep {
        ActionSketchStep::ToolCall {
            step_id: format!("s_test_{tool}"),
            tool: tool.into(),
            args,
            captures_pre: vec![],
            captures: vec![],
            expected_world_model_delta: ExpectedWorldModelDelta::default(),
            requires_approval: None,
        }
    }

    #[test]
    fn repeating_pair_folds_into_loop() {
        let mut s = vec![tc("a", json!({})), tc("a", json!({})), tc("a", json!({}))];
        fold_polling_loops(&mut s);
        assert_eq!(s.len(), 1);
        assert!(matches!(&s[0], ActionSketchStep::Loop { .. }));
    }

    #[test]
    fn distinct_steps_do_not_fold() {
        let mut s = vec![tc("a", json!({})), tc("b", json!({}))];
        fold_polling_loops(&mut s);
        assert_eq!(s.len(), 2);
        assert!(matches!(&s[0], ActionSketchStep::ToolCall { .. }));
    }

    #[test]
    fn folded_loop_records_repeat_count_in_max_iterations() {
        let mut s = vec![
            tc("poll", json!({})),
            tc("poll", json!({})),
            tc("poll", json!({})),
        ];
        fold_polling_loops(&mut s);
        let ActionSketchStep::Loop {
            max_iterations,
            body,
            ..
        } = &s[0]
        else {
            panic!("expected Loop, got {:?}", s[0]);
        };
        // 3 observed iterations + 3-iteration safety margin.
        assert_eq!(*max_iterations, 6);
        assert_eq!(body.len(), 1);
    }
}
