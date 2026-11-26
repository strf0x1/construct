//! The peer popup: approve or deny a join (with the SAS code to compare and
//! its countdown), or answer a peer's permission question.
//!
//! The design choice: it must never pass for the local permission panel. Its
//! badge names the peer, its border breathes cyan and lavender instead of
//! orange, its intro says the question is about the peer's own machine, and
//! "Always" says it is saved to that peer's config, not this host's. Keys are
//! ignored for the same 450 ms after it appears (ADR 0016), and esc puts an
//! item aside rather than answering it: the tile keeps showing it.

use super::{Act, Item, Peers, clean};
use crate::markdown::wrap;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use unicode_width::UnicodeWidthStr;

const BADGE_FG: Color = Color::Rgb(26, 27, 38);

/// One choice: what it sends, what it says, its color and its key.
struct Choice {
    option: String,
    text: String,
    color: Color,
    key: Option<char>,
}

fn choices(item: &Item) -> Vec<Choice> {
    let choice = |option: &str, text: String, color, key| Choice { option: option.into(), text, color, key };
    match item {
        Item::Join(_) => vec![
            choice("approve", "Approve · pins its key".into(), theme::GREEN, Some('a')),
            choice("deny", "Deny".into(), theme::RED, Some('d')),
        ],
        Item::Ask(p, n) => p.questions[*n]
            .options
            .iter()
            .map(|(id, label)| {
                let label = clean(if label.is_empty() { id } else { label });
                let peer = &p.id;
                match id.as_str() {
                    "once" => choice(id, format!("Allow once on {peer}"), theme::GREEN, Some('y')),
                    "session" => choice(id, format!("Allow for {peer}'s session"), theme::GREEN, Some('s')),
                    "always" => {
                        choice(id, format!("Always allow · saved to {peer}'s config"), theme::YELLOW, Some('a'))
                    }
                    "deny" => choice(id, "Deny".into(), theme::RED, Some('n')),
                    i if i.ends_with("always") => {
                        choice(id, format!("{label} · saved to {peer}'s config"), theme::YELLOW, None)
                    }
                    i if i.ends_with("session") => choice(id, format!("{label} on {peer}"), theme::GREEN, None),
                    _ => choice(id, label, theme::DIM, None),
                }
            })
            .collect(),
    }
}

/// The popup's rows `w` columns wide.
fn body(item: &Item, selected: usize, w: usize, now: u64) -> Vec<Line<'static>> {
    let strong = Style::new().fg(Color::Rgb(235, 238, 255)).add_modifier(Modifier::BOLD);
    let mut out = vec![Line::default()];
    let from = |p: &super::PeerRow| p.remote.clone().unwrap_or_else(|| "an unknown address".into());
    match item {
        Item::Join(p) => {
            out.push(Line::from(Span::styled(format!("{} wants to join this hub", p.id), strong)));
            out.push(Line::from(Span::styled(format!("from {}", from(p)), theme::dim())));
            out.push(Line::default());
            out.push(Line::from(vec![
                Span::styled("code  ", theme::dim()),
                Span::styled(
                    p.code.clone().unwrap_or_default(),
                    Style::new().fg(theme::YELLOW).add_modifier(Modifier::BOLD),
                ),
            ]));
            let check = "the peer prints its code too: approve only if they match";
            out.extend(wrap(vec![Span::styled(check, theme::dim())], w, vec![], vec![]));
            if let Some(t) = p.expires {
                out.push(Line::default());
                let left = t.saturating_sub(now);
                out.push(Line::from(Span::styled(
                    format!("expires in {}m {:02}s", left / 60, left % 60),
                    theme::faint(),
                )));
            }
        }
        Item::Ask(p, n) => {
            let intro = format!("{} asks, about its own machine ({}):", p.id, from(p));
            out.extend(wrap(vec![Span::styled(intro, theme::dim())], w, vec![], vec![]));
            out.push(Line::default());
            let mark = Span::styled("  ▸ ", Style::new().fg(theme::CYAN));
            let title = clean(&p.questions[*n].title);
            for para in title.lines() {
                out.extend(wrap(
                    vec![Span::styled(para.to_string(), strong.fg(theme::CYAN))],
                    w,
                    vec![mark.clone()],
                    vec![Span::raw("    ")],
                ));
            }
        }
    }
    out.push(Line::default());
    for (i, c) in choices(item).into_iter().enumerate() {
        let on = i == selected;
        let bg = if on { theme::SELECTED_BG } else { Color::Reset };
        let key = c.key.map(|k| format!("{k} ")).unwrap_or_default();
        let mut style = Style::new().fg(c.color).bg(bg);
        if on {
            style = style.add_modifier(Modifier::BOLD);
        }
        // What is approved wraps, never cut: a folder's whole path stays readable.
        let mark =
            Span::styled(if on { " ❯ " } else { "   " }, Style::new().fg(c.color).bg(bg).add_modifier(Modifier::BOLD));
        let hang = Span::styled("   ", Style::new().bg(bg));
        let room = w.saturating_sub(2).max(8);
        for (n, mut line) in
            wrap(vec![Span::styled(c.text, style)], room, vec![mark], vec![hang]).into_iter().enumerate()
        {
            let end = if n == 0 { key.clone() } else { String::new() };
            let fill = w.saturating_sub(line.width() + end.width());
            line.spans.push(Span::styled(" ".repeat(fill), Style::new().bg(bg)));
            line.spans.push(Span::styled(end, Style::new().fg(theme::DIM).bg(bg)));
            out.push(line);
        }
    }
    out
}

/// Draw the popup for `item`, centered in `area`.
pub(super) fn render(f: &mut Frame, area: Rect, item: &Item, selected: usize, ready: bool, phase: f32, now: u64) {
    let width = area.width.saturating_sub(4).min(76);
    let inner_w = width.saturating_sub(4) as usize;
    let lines = body(item, selected, inner_w, now);
    let height = (lines.len() as u16 + 2).min(area.height);
    let at = Rect::new(area.x + (area.width - width) / 2, area.y + (area.height - height) / 2, width, height);
    let edge = theme::pulse(phase, theme::CYAN, theme::ACCENT2);
    let (what, id) = match item {
        Item::Join(p) => ("JOIN", &p.id),
        Item::Ask(p, _) => ("PEER", &p.id),
    };
    let badge =
        Span::styled(format!(" ⇄ {what} · {id} "), Style::new().fg(BADGE_FG).bg(edge).add_modifier(Modifier::BOLD));
    let hint = if ready { " ↑↓ select · enter confirm · esc later " } else { " … " };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(edge))
        .padding(Padding::horizontal(1))
        .title_top(Line::from(vec![badge, Span::raw(" ")]))
        .title_bottom(Line::from(Span::styled(hint, theme::dim())));
    let inner = block.inner(at);
    f.render_widget(Clear, at);
    f.render_widget(block, at);
    f.render_widget(Paragraph::new(lines), inner);
}

/// A key for the peer popup or the attach view; true when it was used here.
/// While the popup is up it takes every key, so typing can't reach the
/// input line or answer by accident.
pub(crate) fn key(peers: &mut Peers, k: KeyEvent) -> bool {
    let Some(item) = peers.current() else {
        return peers.attach.is_some() && super::attach::key(peers, k);
    };
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    if k.code == KeyCode::Esc || (ctrl && k.code == KeyCode::Char('c')) {
        peers.set_aside.insert(item.key());
        peers.current();
        return true;
    }
    if !peers.shown.as_ref().is_some_and(|s| s.1.elapsed() >= theme::GRACE) {
        return true;
    }
    let options = choices(&item);
    let n = options.len().max(1);
    let pick = match k.code {
        KeyCode::Up | KeyCode::Char('k') => {
            peers.selected = (peers.selected + n - 1) % n;
            None
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
            peers.selected = (peers.selected + 1) % n;
            None
        }
        KeyCode::Enter => options.get(peers.selected),
        KeyCode::Char(c) if !ctrl => options.iter().find(|o| o.key == Some(c.to_ascii_lowercase())),
        _ => None,
    };
    let Some(pick) = pick else { return true };
    let act = match &item {
        Item::Join(p) if pick.option == "approve" => Act::Approve(p.id.clone()),
        Item::Join(p) => Act::Deny(p.id.clone()),
        Item::Ask(p, q) => Act::Answer {
            id: p.id.clone(),
            question: q + 1,
            title: p.questions[*q].title.clone(),
            option: pick.option.clone(),
        },
    };
    peers.send(act);
    peers.set_aside.insert(item.key());
    peers.current();
    true
}

#[cfg(test)]
mod tests {
    use super::super::tests::row;
    use super::super::{Question, Standing};
    use super::*;

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn always_says_whose_config_it_is_saved_to() {
        let mut p = row("old-pc", Standing::Pinned);
        let opts = ["once", "session", "always", "deny"].map(|o| (o.to_string(), format!("peer says {o}")));
        p.questions = vec![Question { title: "read /etc/shadow\x1b[2J".into(), options: opts.to_vec() }];
        let rows = text(&body(&Item::Ask(p, 0), 0, 60, 0));
        assert!(rows.iter().any(|r| r.contains("old-pc asks, about its own machine (10.0.0.5:5000)")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("read /etc/shadow\\u{1b}[2J")), "escaped: {rows:#?}");
        assert!(rows.iter().any(|r| r.contains("Always allow · saved to old-pc's config")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("❯ Allow once on old-pc")), "{rows:#?}");
        assert!(rows.iter().all(|r| r.width() <= 60), "{rows:#?}");
    }

    #[test]
    fn a_long_folder_choice_wraps_instead_of_being_cut() {
        let mut p = row("old-pc", Standing::Pinned);
        let folder = format!("/very/long/{}", "directory/".repeat(10));
        let opts = vec![("folder-always".to_string(), format!("Always allow {folder}"))];
        p.questions = vec![Question { title: "read".into(), options: opts }];
        let rows = text(&body(&Item::Ask(p, 0), 0, 50, 0));
        let joined: String = rows.concat().split_whitespace().collect();
        assert!(joined.contains(&folder) && joined.contains("savedtoold-pc'sconfig"), "{rows:#?}");
        assert!(rows.iter().all(|r| r.width() <= 50), "{rows:#?}");
    }

    #[test]
    fn a_join_shows_its_code_and_countdown() {
        let p = PeerRow { code: Some("K7M-4QP".into()), expires: Some(400), ..row("old-pc", Standing::Pending) };
        let rows = text(&body(&Item::Join(p), 1, 60, 109));
        assert!(rows.iter().any(|r| r == "code  K7M-4QP"), "{rows:#?}");
        assert!(rows.iter().any(|r| r == "expires in 4m 51s"), "{rows:#?}");
        assert!(rows.iter().any(|r| r.starts_with(" ❯ Deny")), "{rows:#?}");
    }

    use super::super::PeerRow;
}
