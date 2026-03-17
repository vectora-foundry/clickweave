#![cfg(test)]

use super::types::{WalkthroughEvent, WalkthroughEventKind};
use uuid::Uuid;

pub fn make_event(timestamp: u64, kind: WalkthroughEventKind) -> WalkthroughEvent {
    WalkthroughEvent {
        id: Uuid::new_v4(),
        timestamp,
        kind,
    }
}
