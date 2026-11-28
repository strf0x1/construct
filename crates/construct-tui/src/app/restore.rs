//! Historical UI projection, separate from the compacted model messages.
//! A ready notification atomically replaces all session-specific UI state.

use super::App;
use crate::theme;
use crate::transcript::Item;
use crate::worker::Ready;

impl App {
    pub(crate) fn restore(&mut self, mut ready: Ready) {
        let ack = ready.ack.take();
        // The first frame is interactive before initialization finishes.
        // Preserve everything the user has already typed or opened there.
        let first = !self.ready;
        let initial_picker = if first { self.picker.take() } else { None };
        self.turn.restored(first);
        self.learning = None;
        self.toast = None;
        self.prompts.clear();
        self.jobs = Some(ready.jobs);
        self.generation = ready.tag;
        self.session = ready.session;
        self.cfg = ready.cfg;
        self.catalog = Some(ready.catalog);
        self.canceller = Some(ready.canceller);
        self.groups = ready.groups;
        self.ready = true;
        self.picker = None;
        let (history, jobs_running) = self.transcript.replay(&ready.entries, self.catalog.as_deref(), first);
        self.jobs_running = jobs_running;
        self.composer.replace_history(history, first);
        if first {
            self.picker = initial_picker;
            if let Some(p) = self.picker.as_mut() {
                p.refresh(&self.cfg);
            }
        }
        self.totals = ready.spent;
        self.context_pct = self.context_pct_now();
        self.refresh_palette();
        if let Some(ack) = ack {
            let _ = ack.send(());
        }
    }

    pub(super) fn switch_blocked(&mut self) -> bool {
        let blocked = self.turn.blocks_switch()
            || !self.prompts.is_empty()
            || self.jobs.as_ref().is_some_and(|j| j.running() > 0);
        if blocked {
            self.transcript.push(Item::Notice(
                theme::YELLOW,
                "Cannot switch sessions while a turn, queued prompt, permission request or background job is active. Wait or cancel it first."
                    .into(),
            ));
        }
        blocked
    }
}
