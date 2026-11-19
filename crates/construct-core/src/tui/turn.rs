//! Whether the TUI is idle, switching sessions, or has turns in flight, as one
//! value with every transition here. The count of user turns sent to the
//! worker is settled in-band: a turn that `Submitted` marks as the user's
//! takes one off at `TurnEnd`, a wake turn takes none, and a command the
//! worker refused without a turn takes one off through `refused`.

use std::time::Instant;

/// What a running turn is doing, for the activity line.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Phase {
    Routing,
    Waiting,
    Thinking,
    Writing,
    Tool(String),
}

/// The turn on screen, between `TurnStart` and `TurnEnd`.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Running {
    pub(super) phase: Phase,
    pub(super) started: Instant,
    /// Characters of reply streamed so far, for the chars/s readout.
    pub(super) chars: usize,
    /// `Submitted` said this turn answers a queued user command; a wake turn
    /// (job completions) never does.
    user: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Turn {
    Idle,
    /// `/resume`, `/new` or a picked session is being prepared; keys wait.
    Switching,
    /// `queued` user commands are sent and unanswered (the running one
    /// included when it is the user's); `running` is the turn on screen.
    Busy {
        queued: usize,
        running: Option<Running>,
    },
}

impl Turn {
    /// The user sent a prompt or `/skill` line to the worker.
    pub(super) fn submit(&mut self) {
        *self = match std::mem::replace(self, Turn::Idle) {
            Turn::Busy { queued, running } => Turn::Busy { queued: queued + 1, running },
            Turn::Idle | Turn::Switching => Turn::Busy { queued: 1, running: None },
        };
    }

    /// A session switch was sent. Callers check `blocks_switch` first.
    pub(super) fn switch(&mut self) {
        *self = Turn::Switching;
    }

    /// `TurnStart`: a turn is on screen, the user's or a wake.
    pub(super) fn start(&mut self, now: Instant) {
        let queued = match self {
            Turn::Busy { queued, .. } => *queued,
            Turn::Idle | Turn::Switching => 0,
        };
        let running = Running { phase: Phase::Routing, started: now, chars: 0, user: false };
        *self = Turn::Busy { queued, running: Some(running) };
    }

    /// `Submitted`: the running turn is the user's.
    pub(super) fn submitted(&mut self) {
        if let Some(r) = self.running_mut() {
            r.user = true;
        }
    }

    /// `TurnEnd`: the turn is over, and a user turn answers its command.
    pub(super) fn end(&mut self) {
        let Turn::Busy { queued, running } = self else { return };
        let user = running.take().is_some_and(|r| r.user);
        let left = if user { queued.saturating_sub(1) } else { *queued };
        *self = if left == 0 { Turn::Idle } else { Turn::Busy { queued: left, running: None } };
    }

    /// The worker answered a command without running a turn (no session, or
    /// the skill was gone by the time it ran).
    pub(super) fn refused(&mut self) {
        let Turn::Busy { queued, running } = self else { return };
        let left = queued.saturating_sub(1);
        *self = match running.take() {
            None if left == 0 => Turn::Idle,
            running => Turn::Busy { queued: left, running },
        };
    }

    /// An `Error`: a switch that was waiting on it failed. A turn's own error
    /// is always followed by its `TurnEnd`.
    pub(super) fn error(&mut self) {
        if *self == Turn::Switching {
            *self = Turn::Idle;
        }
    }

    /// A session landed. Before the first one, commands typed on the first
    /// frame are still queued in the worker and will run in this session.
    pub(super) fn restored(&mut self, first: bool) {
        *self = match self {
            Turn::Busy { queued, .. } if first && *queued > 0 => Turn::Busy { queued: *queued, running: None },
            _ => Turn::Idle,
        };
    }

    pub(super) fn set_phase(&mut self, phase: Phase) {
        if let Some(r) = self.running_mut() {
            r.phase = phase;
        }
    }

    /// The stream opened: thinking, unless it already got further.
    pub(super) fn opened(&mut self) {
        if let Some(r) = self.running_mut()
            && matches!(r.phase, Phase::Routing | Phase::Waiting)
        {
            r.phase = Phase::Thinking;
        }
    }

    /// Reply text streamed.
    pub(super) fn wrote(&mut self, chars: usize) {
        if let Some(r) = self.running_mut() {
            r.phase = Phase::Writing;
            r.chars += chars;
        }
    }

    /// A turn is running or queued: esc interrupts, input queues.
    pub(super) fn busy(&self) -> bool {
        matches!(self, Turn::Busy { .. })
    }

    /// Keys other than overlays' wait for the switch to land.
    pub(super) fn switching(&self) -> bool {
        *self == Turn::Switching
    }

    pub(super) fn blocks_switch(&self) -> bool {
        *self != Turn::Idle
    }

    pub(super) fn running(&self) -> Option<&Running> {
        match self {
            Turn::Busy { running, .. } => running.as_ref(),
            Turn::Idle | Turn::Switching => None,
        }
    }

    fn running_mut(&mut self) -> Option<&mut Running> {
        match self {
            Turn::Busy { running, .. } => running.as_mut(),
            Turn::Idle | Turn::Switching => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued(t: &Turn) -> usize {
        match t {
            Turn::Busy { queued, .. } => *queued,
            _ => 0,
        }
    }

    #[test]
    fn a_user_turn_settles_its_command() {
        let mut t = Turn::Idle;
        t.submit();
        assert!(t.busy() && t.running().is_none());
        t.start(Instant::now());
        t.submitted();
        t.end();
        assert_eq!(t, Turn::Idle);
    }

    #[test]
    fn a_wake_turn_leaves_the_queue_alone() {
        let mut t = Turn::Idle;
        t.start(Instant::now());
        t.submit(); // typed while the wake runs
        t.end();
        assert_eq!(queued(&t), 1);
        assert!(t.busy() && t.running().is_none());
        t.start(Instant::now());
        t.submitted();
        t.end();
        assert_eq!(t, Turn::Idle);
    }

    #[test]
    fn a_queued_message_keeps_it_busy_between_turns() {
        let mut t = Turn::Idle;
        t.submit();
        t.start(Instant::now());
        t.submitted();
        t.submit();
        t.end();
        assert_eq!(queued(&t), 1);
        assert!(t.blocks_switch());
    }

    #[test]
    fn a_refused_command_does_not_leave_it_busy() {
        // `/skill` whose SKILL.md went away: an error, no TurnStart or TurnEnd.
        let mut t = Turn::Idle;
        t.submit();
        t.error();
        assert!(t.busy());
        t.refused();
        assert_eq!(t, Turn::Idle);
    }

    #[test]
    fn a_refusal_behind_a_running_turn_keeps_the_turn() {
        let mut t = Turn::Idle;
        t.start(Instant::now());
        t.submit();
        t.refused();
        assert!(t.running().is_some());
        t.end();
        assert_eq!(t, Turn::Idle);
    }

    #[test]
    fn a_failed_switch_unblocks_input() {
        let mut t = Turn::Idle;
        t.switch();
        assert!(t.switching() && !t.busy() && t.blocks_switch());
        t.error();
        assert_eq!(t, Turn::Idle);
    }

    #[test]
    fn the_first_session_keeps_what_was_typed_before_it() {
        let mut t = Turn::Idle;
        t.submit();
        t.restored(true);
        assert_eq!(queued(&t), 1);
        t.restored(false);
        assert_eq!(t, Turn::Idle);
    }

    #[test]
    fn phases_move_only_while_a_turn_runs() {
        let mut t = Turn::Idle;
        t.set_phase(Phase::Waiting);
        t.wrote(5);
        assert_eq!(t, Turn::Idle);
        t.start(Instant::now());
        t.set_phase(Phase::Tool("shell".into()));
        t.opened();
        assert_eq!(t.running().map(|r| r.phase.clone()), Some(Phase::Tool("shell".into())));
        t.set_phase(Phase::Waiting);
        t.opened();
        t.wrote(3);
        let r = t.running().cloned();
        assert_eq!(r.map(|r| (r.phase, r.chars)), Some((Phase::Writing, 3)));
    }
}
