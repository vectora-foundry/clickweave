#![cfg(test)]

use crate::{Condition, LiteralValue, Operator, Position, ValueRef};

pub fn pos(x: f32, y: f32) -> Position {
    Position { x, y }
}

pub fn dummy_condition() -> Condition {
    Condition {
        left: ValueRef::Literal {
            value: LiteralValue::Bool { value: true },
        },
        operator: Operator::Equals,
        right: ValueRef::Literal {
            value: LiteralValue::Bool { value: true },
        },
    }
}
