//! `/peer` and the attach view: list the peers, send one a prompt, switch
//! its model (`--model`, #390), or watch its session over the transcript
//! while the input line stays yours.
//!
//! The design choice: the view is read-only and lies over the transcript's
//! rows only, so `/peer <name> <prompt>` can be typed while watching, and
//! esc (or pgup/pgdn to scroll) are the only keys it takes.

use super::tile::{Tile, lines};
use super::{Act, Attach, Peers, Standing, clean, now};
use crate::markdown::wrap;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

/// `/peer`, `/peer <name>`, `/peer <name> <prompt>`,
/// `/peer <name> --model [<selector> | --clear]`; what to say about it.
pub(crate) fn command(peers: &mut Peers, arg: &str) -> Option<(Color, String)> {
    let (id, text) = arg.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((arg, ""));
    if peers.acts.is_none() {
        return Some((theme::DIM, "no peers: this build has no hub".into()));
    }
    if id.is_empty() {
        let list = lines(&peers.rows, now());
        if list.is_empty() {
            let none = "no peers yet (is `construct hub` running? `construct hub invite <id>` adds one)";
            return Some((theme::DIM, none.into()));
        }
        let rows: Vec<String> = list.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        let how = "/peer <name> watches one · /peer <name> <prompt> sends · /peer <name> --model <model> switches";
        return Some((theme::DIM, format!("peers:\n{}\n{how}", rows.join("\n"))));
    }
    let Some(row) = peers.row(id) else {
        return Some((theme::YELLOW, format!("no peer {id} — /peer lists them")));
    };
    if let Some(rest) = text.strip_prefix("--model").filter(|r| r.is_empty() || r.starts_with(' ')) {
        return model(peers, id, rest.trim());
    }
    if text.is_empty() {
        // Whatever it is waiting on comes back to the popup.
        peers.set_aside.retain(|k| !k.contains(&format!("\0{id}\0")));
        peers.attach(id);
        return None;
    }
    if row.standing != Standing::Pinned {
        let state = Tile::of(row, now()).word();
        return Some((theme::YELLOW, format!("{id} is {state}: only a joined peer takes prompts")));
    }
    peers.send(Act::Prompt { id: id.to_string(), text: text.to_string() });
    None
}

/// `/peer <id> --model …`: show the model, set it, or `--clear` it.
fn model(peers: &Peers, id: &str, arg: &str) -> Option<(Color, String)> {
    let row = peers.row(id)?;
    match arg {
        "" => {
            let shown = match &row.model {
                Some((m, true)) => format!("{id}: {m}, set at the hub · /peer {id} --model --clear goes back"),
                Some((m, false)) => format!("{id}: {m}, its own"),
                None => format!("{id}: its own model (no model call through the hub yet)"),
            };
            Some((theme::DIM, shown))
        }
        _ if row.standing == Standing::Revoked => Some((theme::YELLOW, format!("{id} is revoked"))),
        "--clear" => {
            peers.send(Act::Model { id: id.to_string(), model: None });
            None
        }
        m => {
            peers.send(Act::Model { id: id.to_string(), model: Some(m.to_string()) });
            None
        }
    }
}

/// A key while the attach view is open.
pub(super) fn key(peers: &mut Peers, k: KeyEvent) -> bool {
    let Some(a) = peers.attach.as_mut() else { return false };
    match k.code {
        KeyCode::Esc => peers.detach(),
        KeyCode::PageUp => a.scroll = a.scroll.saturating_add(10),
        KeyCode::PageDown => a.scroll = a.scroll.saturating_sub(10),
        _ => return false,
    }
    true
}

/// Draw the attach view over `area`, the transcript's rows.
pub(super) fn render(f: &mut Frame, area: Rect, a: &mut Attach, state: Option<Tile>) {
    let state = state.map_or("gone", Tile::word);
    let title =
        Span::styled(format!(" ⇄ {} · {state} ", a.id), Style::new().fg(theme::CYAN).add_modifier(Modifier::BOLD));
    let hint = format!(" esc close · pgup/pgdn scroll · /peer {} <prompt> sends ", a.id);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::CYAN))
        .padding(Padding::horizontal(1))
        .title_top(Line::from(title))
        .title_bottom(Line::from(Span::styled(hint, theme::dim())));
    let inner = block.inner(area);
    let text = if a.text.is_empty() { "loading…".to_string() } else { clean(&a.text) };
    let w = inner.width as usize;
    let body: Vec<Line<'static>> =
        text.lines().flat_map(|l| wrap(vec![Span::raw(l.to_string())], w, vec![], vec![])).collect();
    // Pinned to the bottom, like the transcript; pgup walks back.
    let bottom = body.len().saturating_sub(inner.height as usize);
    a.scroll = a.scroll.min(bottom);
    let shown: Vec<Line<'static>> = body.into_iter().skip(bottom - a.scroll).take(inner.height as usize).collect();
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(shown), inner);
}
