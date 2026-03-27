#![cfg(test)]

use crate::output_schema::{ConditionValue, OutputRef};
use crate::{Condition, LiteralValue, Operator, Position};

pub fn pos(x: f32, y: f32) -> Position {
    Position { x, y }
}

pub fn dummy_condition() -> Condition {
    Condition {
        left: OutputRef {
            node: "click_1".to_string(),
            field: "result".to_string(),
        },
        operator: Operator::Equals,
        right: ConditionValue::Literal {
            value: LiteralValue::Bool { value: true },
        },
    }
}
