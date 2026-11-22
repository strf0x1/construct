//! The input line as drawn: the text (or its placeholder) wrapped under the
//! `› ` prompt, and where the cursor lands in it. Columns are counted as
//! drawn, so a pasted tab takes no column.

use super::app::App;
use super::theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

impl App {
    pub(super) fn input_lines(&self, width: usize) -> (Vec<Line<'static>>, (u16, u16)) {
        let prompt_w = 2;
        let avail = width.saturating_sub(prompt_w + 1).max(4);
        let mut lines = vec![];
        let mut cur = (0u16, prompt_w as u16);
        let mut row = 0u16;
        let mut offset = 0usize;
        let placeholder = self.input.is_empty();
        let text = if placeholder {
            if self.turn.busy() { "queue a message… (esc to interrupt)" } else { "ask construct anything… (/help)" }
                .to_string()
        } else {
            self.input.clone()
        };
        for (pi, para) in text.split('\n').enumerate() {
            let mut chunk = String::new();
            let mut cw = 0;
            let mut chunks: Vec<(String, usize)> = vec![];
            let mut start = offset;
            for (bi, c) in para.char_indices() {
                let w = crate::text::cols_after(para.get(..bi).and_then(|p| p.chars().next_back()), c);
                if cw + w > avail {
                    chunks.push((std::mem::take(&mut chunk), start));
                    start = offset + bi;
                    cw = 0;
                }
                chunk.push(c);
                cw += w;
            }
            chunks.push((chunk, start));
            for (ci, (c, s)) in chunks.iter().enumerate() {
                let prefix = if pi == 0 && ci == 0 {
                    Span::styled("› ", Style::new().fg(theme::ACCENT2).add_modifier(Modifier::BOLD))
                } else {
                    Span::raw("  ")
                };
                let style = if placeholder { theme::faint() } else { Style::default() };
                lines.push(Line::from(vec![prefix, Span::styled(c.clone(), style)]));
                if !placeholder && self.cursor >= *s && self.cursor <= s + c.len() {
                    let before = self.input.get(*s..self.cursor).unwrap_or("");
                    cur = (row, (prompt_w + crate::text::cols(before)) as u16); // as drawn: a pasted tab is no column
                }
                row += 1;
            }
            offset += para.len() + 1;
        }
        (lines, cur)
    }
}
