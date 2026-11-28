//! Turn lifecycle state machine for one ACP [`Session`]: tracks turn count,
//! `working` / `waiting` / `idle` phases, and stop reasons.

use crate::event::Event;
use serde_json::{Value, json};

pub(super) struct Turn {
    pub number: u64,
    pub phase: Phase,
}

#[derive(PartialEq, Eq)]
pub(super) enum Phase {
    Working,
    Waiting,
    Idle(Option<String>),
}

impl Turn {
    pub fn new() -> Self {
        Self { number: 0, phase: Phase::Idle(None) }
    }

    pub fn observe(&mut self, event: &Event, reason: &mut Option<&'static str>) {
        match event {
            Event::TurnStart => {
                self.number += 1;
                *reason = None;
            }
            Event::Error { message } => {
                if message.ends_with("(max_steps)") {
                    *reason = Some("max_turn_requests");
                } else if reason.is_none() {
                    *reason = Some("error");
                }
            }
            Event::TurnEnd { interrupted, .. } => {
                let stop = if *interrupted { "cancelled" } else { reason.unwrap_or("end_turn") };
                self.phase = Phase::Idle(Some(stop.into()));
            }
            Event::Recovered { .. } => self.phase = Phase::Idle(Some("cancelled".into())),
            _ => {}
        }
    }

    pub fn is_idle(&self) -> bool {
        matches!(self.phase, Phase::Idle(_))
    }

    pub fn value(&self, session: &str) -> Value {
        let state = match self.phase {
            Phase::Working => "working",
            Phase::Waiting => "waiting",
            Phase::Idle(_) => "idle",
        };
        let mut value = json!({ "sessionId": session, "turn": self.number, "state": state });
        if let Phase::Idle(Some(reason)) = &self.phase {
            value["stopReason"] = json!(reason);
        }
        value
    }
}
