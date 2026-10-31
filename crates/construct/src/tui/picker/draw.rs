//! Drawing the picker: a rounded box centered over the transcript, a search
//! line, a table of models and, when there is room, the highlighted model's
//! details beside it. Real ratatui widgets (`Table`, `Paragraph`, `Block`)
//! rather than hand-padded lines: the table scrolls itself to the selection.

use super::{Picker, Row};
use crate::models::{Pricing, short_tokens};
use crate::tui::theme;
use crate::tui::view::fit;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Clear, Padding, Paragraph, Row as TRow, Table, Wrap};
use unicode_width::UnicodeWidthStr;

const SELECTED_BG: Color = Color::Rgb(44, 48, 72);
const CHIP_BG: Color = Color::Rgb(36, 40, 59);
/// Inner width from which the details sit beside the list.
const SPLIT_FROM: u16 = 86;
const DETAIL_W: u16 = 38;
const MAX_W: u16 = 112;
const MAX_H: u16 = 26;

/// Draw the picker over `area` and park the cursor in its search line.
pub(in crate::tui) fn render(f: &mut Frame, area: Rect, p: &mut Picker) {
    let w = area.width.saturating_sub(2).clamp(20, MAX_W);
    let want = (p.rows.len() as u16 + 6).clamp(20, MAX_H);
    let h = area.height.saturating_sub(2).min(want).max(6);
    let r = Rect::new(area.x + area.width.saturating_sub(w) / 2, area.y + area.height.saturating_sub(h) / 2, w, h)
        .intersection(area);
    // A modal: the transcript behind it is wiped, not left peeking out at the edges.
    f.render_widget(Clear, area);
    let edge = Style::new().fg(theme::ACCENT);
    let count = if p.shown.len() == p.rows.len() {
        format!(" {} ", p.rows.len())
    } else {
        format!(" {}/{} ", p.shown.len(), p.rows.len())
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(edge)
        .padding(Padding::horizontal(1))
        .title(Line::from(vec![
            Span::styled(" ◆ ", Style::new().fg(theme::ACCENT2)),
            Span::styled("models ", edge.add_modifier(Modifier::BOLD)),
        ]))
        .title_top(Line::from(Span::styled(count, theme::faint())).right_aligned())
        .title_bottom(Line::from(Span::styled(" ↑↓ select · enter switch · esc close ", theme::faint())));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let [search, rule, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)]).areas(inner);

    let (q, qstyle) = if p.query.is_empty() {
        ("search models…", theme::faint())
    } else {
        (p.query.as_str(), Style::new().fg(Color::Rgb(235, 238, 255)))
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled("⌕ ", Style::new().fg(theme::ACCENT2)), Span::styled(q, qstyle)])),
        search,
    );
    f.render_widget(Paragraph::new(Line::from(Span::styled("─".repeat(rule.width as usize), theme::faint()))), rule);

    let split = inner.width >= SPLIT_FROM;
    let [list, side] = if split {
        Layout::horizontal([Constraint::Min(30), Constraint::Length(DETAIL_W)]).spacing(2).areas(body)
    } else {
        [body, Rect::default()]
    };
    list_table(f, list, p);
    if split && let Some(row) = p.current() {
        f.render_widget(
            Paragraph::new(details(row, p.groups.get(&row.provider).map(|(_, has)| has.as_str())))
                .wrap(Wrap { trim: false })
                .block(side_block()),
            side,
        );
    }
    f.set_cursor_position(Position::new(search.x + 2 + p.query.width() as u16, search.y));
}

fn side_block() -> Block<'static> {
    Block::new().borders(Borders::LEFT).border_style(theme::faint()).padding(Padding::left(1))
}

fn list_table(f: &mut Frame, area: Rect, p: &mut Picker) {
    if p.shown.is_empty() {
        f.render_widget(Paragraph::new(Line::from(Span::styled("  no model matches", theme::dim()))), area);
        return;
    }
    // marker, name, window, price, running-model check; one blank between each
    let (marker, ctx, price, check) = (1u16, 6u16, 11u16, 1u16);
    let name_w = area.width.saturating_sub(marker + ctx + price + check + 4) as usize;
    let head = Style::new().fg(theme::DIM);
    let header = TRow::new([
        Cell::from(""),
        Cell::from(Span::styled("Model", head)),
        Cell::from(Line::from(Span::styled("Ctx", head)).right_aligned()),
        Cell::from(Line::from(Span::styled("$/M in·out", head)).right_aligned()),
        Cell::from(""),
    ]);
    let lines = p.lines();
    let at = lines.iter().position(|l| *l == Err(p.selected)).unwrap_or(0);
    let rows: Vec<TRow> = lines
        .iter()
        .map(|line| {
            let n = match *line {
                Ok(provider) => {
                    let text = p.groups.get(provider).map_or(provider, |(title, _)| title.as_str());
                    return TRow::new([Cell::from(""), Cell::from(Span::styled(fit(text, name_w), theme::faint()))]);
                }
                Err(n) => n,
            };
            let r = &p.rows[p.shown[n]];
            let sel = n == p.selected;
            let mark = if sel { Span::styled("▸", Style::new().fg(theme::ACCENT2)) } else { Span::raw(" ") };
            let id = if sel { Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD) } else { theme::accent() };
            TRow::new([
                Cell::from(mark),
                Cell::from(name_cell(r, name_w, id)),
                Cell::from(
                    Line::from(Span::styled(short_tokens(r.window), Style::new().fg(theme::CYAN))).right_aligned(),
                ),
                Cell::from(Line::from(price_span(r.pricing)).right_aligned()),
                Cell::from(Span::styled(if r.current { "✓" } else { " " }, Style::new().fg(theme::GREEN))),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(marker),
        Constraint::Min(8),
        Constraint::Length(ctx),
        Constraint::Length(price),
        Constraint::Length(check),
    ];
    let table =
        Table::new(rows, widths).header(header).column_spacing(1).row_highlight_style(Style::new().bg(SELECTED_BG));
    p.table.select(Some(at));
    f.render_stateful_widget(table, area, &mut p.table);
}

/// The id, then the label in what room is left, cut with an ellipsis.
fn name_cell(r: &Row, room: usize, id_style: Style) -> Line<'static> {
    let id = fit(&r.id, room);
    let left = room.saturating_sub(id.width() + 2);
    let mut spans = vec![Span::styled(id, id_style)];
    if !r.label.is_empty() && left >= 6 {
        spans.push(Span::styled(format!("  {}", fit(&r.label, left)), theme::dim()));
    }
    Line::from(spans)
}

fn price_span(p: Option<Pricing>) -> Span<'static> {
    match p {
        None => Span::styled("—", theme::faint()),
        Some(p) if p.prompt == 0.0 && p.completion == 0.0 => Span::styled("free", Style::new().fg(theme::GREEN)),
        Some(p) => Span::styled(format!("${}·{}", money(p.prompt), money(p.completion)), theme::dim()),
    }
}

/// `5`, `0.15`, `2.5`: no trailing zeros.
fn money(x: f64) -> String {
    let s = format!("{x:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The pane beside the list: name, chips, description, then the numbers
/// that decide whether to switch.
fn details(r: &Row, provider: Option<&str>) -> Vec<Line<'static>> {
    let title = if r.label.is_empty() { r.id.clone() } else { r.label.clone() };
    let mut out = vec![Line::from(Span::styled(title, Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD)))];
    if !r.label.is_empty() {
        out.push(Line::from(Span::styled(r.id.clone(), theme::dim())));
    }
    if !r.tags.is_empty() {
        let mut chips = vec![];
        for t in &r.tags {
            chips.push(Span::styled(format!(" {t} "), Style::new().fg(theme::CYAN).bg(CHIP_BG)));
            chips.push(Span::raw(" "));
        }
        out.push(Line::default());
        out.push(Line::from(chips));
    }
    if !r.description.is_empty() {
        out.push(Line::default());
        out.push(Line::from(Span::styled(r.description.clone(), theme::dim())));
    }
    out.push(Line::default());
    let kv = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k:<12}"), theme::faint()),
            Span::styled(v, Style::new().fg(theme::CYAN)),
        ])
    };
    out.push(kv("Context", format!("{} tokens", short_tokens(r.window))));
    out.push(kv("Compacts at", format!("~{}", short_tokens(r.compacts_at))));
    out.push(kv("Max output", r.max_tokens.map_or("provider default".into(), |m| short_tokens(m as u64))));
    out.push(kv(
        "Price",
        match r.pricing {
            None => "not set".into(),
            Some(p) if p.prompt == 0.0 && p.completion == 0.0 => "free".into(),
            Some(p) => format!("${} in · ${} out per M", money(p.prompt), money(p.completion)),
        },
    ));
    if let Some(has) = provider {
        out.push(kv("Provider", r.provider.clone()));
        out.push(kv("Models", has.to_string()));
    }
    out.push(kv("Endpoint", host(&r.endpoint)));
    out.push(kv("Dialect", r.dialect.to_string()));
    out.push(kv("Key", r.key.clone()));
    out.push(Line::default());
    out.push(if r.current {
        Line::from(Span::styled("✓ running now", Style::new().fg(theme::GREEN)))
    } else if !r.catalogued {
        Line::from(Span::styled("not in the catalog: runs on the defaults", Style::new().fg(theme::YELLOW)))
    } else {
        Line::default()
    });
    out
}

/// `openrouter.ai` out of `https://openrouter.ai/api/v1`.
fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn cfg() -> Config {
        let mut c: Config = toml::from_str(
            r#"
            model = "anthropic/claude-haiku-5.5"
            [models."anthropic/claude-haiku-5.5"]
            label = "Haiku 5.5 — fast recon"
            tags = ["fast"]
            context_window = 200000
            max_tokens = 16000
            pricing = { prompt = 1.0, completion = 5.0 }
            [models."anthropic/claude-opus-5.5"]
            label = "Opus 5.5 — planning"
            context_window = 1000000
            [models."local/qwen"]
            base_url = "http://127.0.0.1:8080/v1"
            pricing = { prompt = 0.0, completion = 0.0 }
            "#,
        )
        .unwrap();
        c.select_model("anthropic/claude-haiku-5.5").unwrap();
        c
    }

    fn screen(width: u16, height: u16, p: &mut Picker) -> String {
        let mut t = Terminal::new(TestBackend::new(width, height)).unwrap();
        t.draw(|f| render(f, f.area(), p)).unwrap();
        let b = t.backend().buffer().clone();
        (0..height)
            .map(|y| (0..width).map(|x| b[(x, y)].symbol().to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_wide_terminal_shows_the_list_and_the_selected_models_limits() {
        let mut p = Picker::open(&cfg()).unwrap();
        let s = screen(120, 30, &mut p);
        for want in ["models", "search models…", "anthropic/claude-opus-5.5", "200k", "1M", "$1·5", "free", "✓"] {
            assert!(s.contains(want), "missing {want:?}\n{s}");
        }
        for want in [
            "Haiku 5.5 — fast recon",
            "fast",
            "Context",
            "200k tokens",
            "Compacts at",
            "$1 in · $5 out per M",
            "openrouter.ai",
        ] {
            assert!(s.contains(want), "details missing {want:?}\n{s}");
        }
    }

    #[test]
    fn a_narrow_terminal_drops_the_details_but_keeps_the_list() {
        let mut p = Picker::open(&cfg()).unwrap();
        let s = screen(60, 20, &mut p);
        assert!(s.contains("anthropic/claude-opus-5.5") && !s.contains("Compacts at"), "{s}");
    }

    #[test]
    fn a_query_with_no_match_says_so_and_fits_a_tiny_terminal() {
        let mut p = Picker::open(&cfg()).unwrap();
        for c in "zzz".chars() {
            p.key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(c),
                crossterm::event::KeyModifiers::NONE,
            ));
        }
        assert!(screen(100, 24, &mut p).contains("no model matches"));
        let _ = screen(24, 8, &mut p);
    }

    #[test]
    fn prices_drop_trailing_zeros() {
        assert_eq!((money(5.0), money(0.15), money(2.5)), ("5".into(), "0.15".into(), "2.5".into()));
        assert_eq!(host("https://openrouter.ai/api/v1"), "openrouter.ai");
        assert_eq!(host("http://127.0.0.1:8080/v1"), "127.0.0.1:8080");
    }
}
