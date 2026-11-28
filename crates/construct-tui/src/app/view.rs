//! Frame layout and synchronized drawing: composes the transcript or splash,
//! permission panel, peers tile/popup/attach, activity line, composer, slash
//! palette, status bar, motion effects and active modal overlays.

use super::App;
use crate::motion::peers as peer_motion;
use crate::motion::rise;
use crate::overlays::peers::{Decision as PeerDecision, Drawn as PeersDrawn, Tile as PeerTile};
use anyhow::Result;
use crossterm::queue;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::io::{Stdout, Write};

impl App {
    pub(crate) fn draw(&mut self, term: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        let phase = self.started.elapsed().as_secs_f32();
        let size = term.size()?;
        let area = Rect::new(0, 0, size.width, size.height);
        let width = area.width;
        self.prompts.prune();
        let (input, cursor) = self.composer.lines(width as usize, self.turn.busy());
        let input_h = (input.len() as u16).clamp(1, 8);
        // The slash palette sits between the input and the status line, and
        // gives way to the transcript on a short terminal.
        let rows = (area.height.saturating_sub(input_h + 6) / 2) as usize;
        let palette = self.composer.palette_lines(width as usize, rows);
        let palette_h = palette.len() as u16;
        // A question takes the place of the activity line, rule, input and palette.
        let panel_h = self.prompts.panel_height(width, area.height.saturating_sub(4));
        let risen = panel_h.map(|h| self.motion.panel_rows(h)); // rows it shows while rising
        let bottom = match risen {
            Some(h) => h + 1, // panel + status
            None => {
                let foot = input_h + palette_h + 3; // activity + rule + input + palette + status
                foot + self.motion.reserve(foot) // an answered panel's room, closing
            }
        };
        let main_h = area.height.saturating_sub(bottom);
        let is_splash = self.transcript.is_empty();
        let splash = is_splash.then(|| self.splash(width, main_h, phase));
        let (visible, start, places) = self.transcript.frame(width, main_h, phase, &mut self.motion, splash);
        let activity = self.activity(phase);
        let status = self.status(width as usize, phase);
        let input_scroll = (input.len() as u16).saturating_sub(input_h);
        let rule_color = self.motion.rule_color(self.turn.busy(), phase);
        let mut motion = std::mem::take(&mut self.motion); // the draw closure reads the rest of self
        motion.turn(self.turn.busy().then(|| self.turn.running().map(|r| &r.phase)));
        let mut stdout = std::io::stdout();
        queue!(stdout, BeginSynchronizedUpdate)?;
        term.draw(|f| {
            // pad from the top so short transcripts sit right above the input;
            // the splash centers itself vertically inside `main_h`.
            let pad = if is_splash { 0 } else { main_h.saturating_sub(visible.len() as u16) };
            // a rising panel is drawn whole, then cut to its top rows at the foot
            let panel = panel_h.zip(risen).filter(|_| !self.prompts.is_empty());
            if let Some((h, shown)) = panel {
                let full = Rect::new(0, area.height.saturating_sub(h + 1), width, h);
                self.prompts.render_front(f, full, phase);
                motion.panel(f.buffer_mut(), full);
                rise(f.buffer_mut(), full, shown);
            }
            f.render_widget(Paragraph::new(visible.clone()), Rect::new(0, pad, width, main_h - pad));
            motion.transcript(f.buffer_mut(), &places, start, pad, main_h - pad, is_splash);
            let peers_drawn = self.peers.draw(f, Rect::new(0, 0, width, main_h), self.prompts.is_empty(), phase);
            motion.peers(f.buffer_mut(), peer_frame(peers_drawn));
            if panel.is_some() {
                f.render_widget(Paragraph::new(status.clone()), Rect::new(0, area.height.saturating_sub(1), width, 1));
                return;
            }
            let low = area.height.saturating_sub(input_h + palette_h + 3);
            f.render_widget(Paragraph::new(activity.clone()), Rect::new(0, low, width, 1));
            f.render_widget(
                Paragraph::new(Line::from(Span::styled("─".repeat(width as usize), Style::new().fg(rule_color)))),
                Rect::new(0, low + 1, width, 1),
            );
            f.render_widget(
                Paragraph::new(input.clone()).scroll((input_scroll, 0)),
                Rect::new(0, low + 2, width, input_h),
            );
            f.render_widget(Paragraph::new(palette.clone()), Rect::new(0, low + 2 + input_h, width, palette_h));
            f.render_widget(Paragraph::new(status.clone()), Rect::new(0, low + 2 + input_h + palette_h, width, 1));
            motion.rule(f.buffer_mut(), Rect::new(0, low + 1, width, 1)); // last: a dissolving panel lies over all
            let cy = cursor.0.saturating_sub(input_scroll).min(input_h - 1);
            f.set_cursor_position(Position::new(cursor.1.min(width - 1), low + 2 + cy));
            let modal_area = Rect::new(0, 0, width, area.height.saturating_sub(1));
            if let Some(p) = self.picker.as_mut() {
                p.render(f, modal_area);
            }
            if let Some(p) = self.browser.as_mut() {
                p.render(f, modal_area);
            }
        })?;
        self.motion = motion;
        queue!(stdout, EndSynchronizedUpdate)?;
        stdout.flush()?;
        Ok(())
    }
}

fn peer_frame(d: PeersDrawn) -> peer_motion::PeerFrame {
    let state = |t| match t {
        PeerTile::Expected => peer_motion::PeerState::Expected,
        PeerTile::Pending => peer_motion::PeerState::Pending,
        PeerTile::Online => peer_motion::PeerState::Online,
        PeerTile::Working => peer_motion::PeerState::Working,
        PeerTile::Away => peer_motion::PeerState::Away,
        PeerTile::Stale => peer_motion::PeerState::Stale,
        PeerTile::Revoked => peer_motion::PeerState::Revoked,
    };
    peer_motion::PeerFrame {
        tile: d.tile.map(|(area, rows)| {
            (
                area,
                rows.into_iter()
                    .map(|r| peer_motion::PeerTileRow {
                        id: r.id,
                        state: state(r.state),
                        questions: r.questions,
                        model: r.model,
                    })
                    .collect(),
            )
        }),
        popup: d.popup.map(|p| peer_motion::PeerPopup { area: p.area, key: p.key, is_join: p.is_join }),
        decision: d.decision.map(|x| match x {
            PeerDecision::Allow { pop } => peer_motion::PeerDecision::Allow { pop },
            PeerDecision::Always { pop } => peer_motion::PeerDecision::Always { pop },
            PeerDecision::Deny { pop } => peer_motion::PeerDecision::Deny { pop },
            PeerDecision::Later => peer_motion::PeerDecision::Later,
        }),
        attach: d.attach.map(|a| peer_motion::PeerAttach {
            area: a.area,
            inner: a.inner,
            id: a.id,
            top: a.top,
            text_len: a.text_len,
            working: a.working,
        }),
    }
}
