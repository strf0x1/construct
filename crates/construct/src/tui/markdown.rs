//! Markdown to wrapped, styled lines. We wrap ourselves (rather than letting
//! a Paragraph do it) so scrolling knows exact line counts.

use super::table::{self, Table};
use super::theme;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// Word-wrap styled spans to `width`, prefixing every line with `indent`
/// (first line) / `hang` (continuations).
pub fn wrap(
    spans: Vec<Span<'static>>,
    width: usize,
    indent: Vec<Span<'static>>,
    hang: Vec<Span<'static>>,
) -> Vec<Line<'static>> {
    let iw = indent.iter().map(|s| s.content.width()).sum::<usize>();
    let hw = hang.iter().map(|s| s.content.width()).sum::<usize>();
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = indent.clone();
    let mut cur_w = iw;
    let mut avail = width.saturating_sub(iw).max(8);
    let mut first = true;
    // Split spans into words that keep their trailing whitespace.
    let mut words: Vec<(String, Style)> = Vec::new();
    for s in spans {
        let mut buf = String::new();
        for c in s.content.chars() {
            buf.push(c);
            if c == ' ' {
                words.push((std::mem::take(&mut buf), s.style));
            }
        }
        if !buf.is_empty() {
            words.push((buf, s.style));
        }
    }
    let flush = |lines: &mut Vec<Line<'static>>, cur: &mut Vec<Span<'static>>| {
        lines.push(Line::from(std::mem::take(cur)));
    };
    for (word, style) in words {
        let ww = word.trim_end_matches(' ').width(); // a trailing U+3000 or U+00A0 takes columns
        let used = cur_w - if first { iw } else { hw };
        if used > 0 && used + ww > avail {
            // trim the trailing space of the line we're closing
            if let Some(last) = cur.last_mut() {
                let t = last.content.trim_end().to_string();
                last.content = t.into();
            }
            flush(&mut lines, &mut cur);
            cur = hang.clone();
            cur_w = hw;
            first = false;
            avail = width.saturating_sub(hw).max(8);
        }
        if ww > avail {
            // A single word longer than the line: hard-break it.
            let mut chunk = String::new();
            let mut cw = cur_w - if first { iw } else { hw };
            for (i, c) in word.char_indices() {
                let w = crate::text::cols_after(word.get(..i).and_then(|p| p.chars().next_back()), c);
                if cw + w > avail {
                    cur.push(Span::styled(std::mem::take(&mut chunk), style));
                    flush(&mut lines, &mut cur);
                    cur = hang.clone();
                    first = false;
                    avail = width.saturating_sub(hw).max(8);
                    cw = 0;
                }
                chunk.push(c);
                cw += w;
            }
            cur_w = cw + if first { iw } else { hw };
            cur.push(Span::styled(chunk, style));
            continue;
        }
        cur_w += word.width();
        cur.push(Span::styled(word, style));
    }
    if cur.len() > if first { indent.len() } else { hang.len() } || lines.is_empty() {
        lines.push(Line::from(cur));
    }
    lines
}

struct Ctx {
    width: usize,
    out: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    styles: Vec<Style>,
    lists: Vec<Option<u64>>,
    item_marker: Option<String>,
    quote: usize,
    code: Option<(String, String)>,
    link: Option<String>,
    table: Option<Table>,
}

impl Ctx {
    fn style(&self) -> Style {
        self.styles.iter().fold(Style::default(), |a, s| a.patch(*s))
    }
    fn prefix(&self) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let mut base = String::new();
        for _ in 0..self.quote {
            base.push_str("│ ");
        }
        let depth = self.lists.len();
        let pad = "  ".repeat(depth.saturating_sub(1));
        let q = Span::styled(base.clone(), theme::faint());
        match &self.item_marker {
            Some(m) => {
                let marker = Span::styled(format!("{pad}{m}"), theme::accent());
                let hang = Span::raw(" ".repeat(pad.width() + m.width()));
                (vec![q.clone(), marker], vec![q, hang])
            }
            None if depth > 0 => {
                let hang = Span::raw(format!("{pad}  "));
                (vec![q.clone(), hang.clone()], vec![q, hang])
            }
            None => (vec![q.clone()], vec![q]),
        }
    }
    fn flush_para(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        let (first, hang) = self.prefix();
        let spans = std::mem::take(&mut self.spans);
        self.out.extend(wrap(spans, self.width, first, hang));
        self.item_marker = None;
    }
    fn blank(&mut self) {
        if self.out.last().is_some_and(|l| !l.spans.is_empty() && l.width() > 0) {
            self.out.push(Line::default());
        }
    }
}

pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut c = Ctx {
        width: width.max(10),
        out: vec![],
        spans: vec![],
        styles: vec![],
        lists: vec![],
        item_marker: None,
        quote: 0,
        code: None,
        link: None,
        table: None,
    };
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for ev in Parser::new_ext(text, opts) {
        if let Some(t) = &mut c.table {
            match ev {
                Event::Text(s) | Event::Code(s) => {
                    t.cell.push_str(&s);
                    continue;
                }
                Event::End(TagEnd::TableCell) => {
                    let cell = std::mem::take(&mut t.cell);
                    t.row.push(cell);
                    continue;
                }
                Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow) => {
                    let row = std::mem::take(&mut t.row);
                    t.rows.push(row);
                    continue;
                }
                Event::End(TagEnd::Table) => {
                    if let Some(t) = c.table.take() {
                        c.out.extend(table::render(&t, c.width));
                    }
                    continue;
                }
                _ => continue,
            }
        }
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level, .. } => {
                    c.flush_para();
                    c.blank();
                    let color = match level {
                        HeadingLevel::H1 => theme::ACCENT2,
                        HeadingLevel::H2 => theme::ACCENT,
                        _ => theme::CYAN,
                    };
                    c.styles.push(Style::new().fg(color).add_modifier(Modifier::BOLD));
                }
                Tag::BlockQuote(_) => {
                    c.flush_para();
                    c.quote += 1;
                    c.styles.push(Style::new().add_modifier(Modifier::ITALIC));
                }
                Tag::CodeBlock(kind) => {
                    c.flush_para();
                    let lang = match kind {
                        CodeBlockKind::Fenced(l) => l.split(',').next().unwrap_or("").to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    c.code = Some((lang, String::new()));
                }
                Tag::List(start) => {
                    c.flush_para();
                    if c.lists.is_empty() {
                        c.blank();
                    }
                    c.lists.push(start);
                }
                Tag::Item => {
                    c.flush_para();
                    let depth = c.lists.len();
                    let marker = match c.lists.last_mut() {
                        Some(Some(n)) => {
                            let m = format!("{n}. ");
                            *n += 1;
                            m
                        }
                        _ => if depth > 1 { "◦ " } else { "• " }.to_string(),
                    };
                    c.item_marker = Some(marker);
                }
                Tag::Emphasis => c.styles.push(Style::new().add_modifier(Modifier::ITALIC)),
                Tag::Strong => c.styles.push(Style::new().add_modifier(Modifier::BOLD)),
                Tag::Strikethrough => c.styles.push(Style::new().add_modifier(Modifier::CROSSED_OUT)),
                Tag::Link { dest_url, .. } => {
                    c.link = Some(dest_url.to_string());
                    c.styles.push(Style::new().fg(theme::CYAN).add_modifier(Modifier::UNDERLINED));
                }
                Tag::Table(align) => {
                    c.flush_para();
                    c.blank();
                    c.table = Some(Table { align, ..Default::default() });
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    c.flush_para();
                    if c.lists.is_empty() {
                        c.out.push(Line::default());
                    }
                }
                TagEnd::Heading(_) => {
                    c.flush_para();
                    c.styles.pop();
                }
                TagEnd::BlockQuote(_) => {
                    c.flush_para();
                    c.quote -= 1;
                    c.styles.pop();
                }
                TagEnd::CodeBlock => {
                    if let Some((lang, code)) = c.code.take() {
                        render_code(&mut c, &lang, &code);
                    }
                }
                TagEnd::List(_) => {
                    c.flush_para();
                    c.lists.pop();
                    if c.lists.is_empty() {
                        c.out.push(Line::default());
                    }
                }
                TagEnd::Item => c.flush_para(),
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                    c.styles.pop();
                }
                TagEnd::Link => {
                    c.styles.pop();
                    if let Some(url) = c.link.take() {
                        let shown: String = c.spans.iter().map(|s| s.content.as_ref()).collect();
                        if !shown.ends_with(&url) {
                            c.spans.push(Span::styled(format!(" ({url})"), theme::faint()));
                        }
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some((_, code)) = &mut c.code {
                    code.push_str(&t);
                } else {
                    let st = c.style();
                    c.spans.push(Span::styled(t.to_string(), st));
                }
            }
            Event::Code(t) => c.spans.push(Span::styled(t.to_string(), Style::new().fg(theme::ORANGE))),
            Event::SoftBreak => c.spans.push(Span::raw(" ")),
            Event::HardBreak => c.flush_para(),
            Event::Rule => {
                c.flush_para();
                c.out.push(Line::from(Span::styled("─".repeat(c.width.min(60)), theme::faint())));
            }
            Event::TaskListMarker(done) => {
                c.item_marker = Some(if done { "☑ ".into() } else { "☐ ".into() });
            }
            Event::Html(h) | Event::InlineHtml(h) => {
                let st = c.style();
                c.spans.push(Span::styled(h.to_string(), st));
            }
            _ => {}
        }
    }
    // An unterminated code fence while streaming: show what we have.
    if let Some((lang, code)) = c.code.take() {
        render_code(&mut c, &lang, &code);
    }
    if let Some(t) = c.table.take() {
        c.out.extend(table::render(&t, c.width));
    }
    c.flush_para();
    while c.out.last().is_some_and(|l| l.width() == 0) {
        c.out.pop();
    }
    c.out
}

fn render_code(c: &mut Ctx, lang: &str, code: &str) {
    let bar = Span::styled("▎ ", Style::new().fg(theme::FAINT));
    if !lang.is_empty() {
        c.out.push(Line::from(vec![bar.clone(), Span::styled(lang.to_string(), theme::faint())]));
    }
    let style = Style::new().fg(theme::GREEN);
    for line in code.trim_end_matches('\n').split('\n') {
        let line = line.replace('\t', "    ");
        c.out.extend(wrap(vec![Span::styled(line, style)], c.width, vec![bar.clone()], vec![bar.clone()]));
    }
    c.out.push(Line::default());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::prop;
    use proptest::prelude::*;

    const SYNTAX: &[&str] = &[
        "*",
        "**",
        "_",
        "`",
        "```",
        "```rust\n",
        "~~",
        "# ",
        "## ",
        "> ",
        "- ",
        "* ",
        "1. ",
        "- [ ] ",
        "- [x] ",
        "|",
        "| --- |",
        "|:-:|",
        "[",
        "](",
        ")",
        "<b>",
        "</b>",
        "\\",
        "---",
        "    ",
        "\n\n",
        "\t",
    ];

    fn plain(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn a_word_ending_in_a_wide_space_still_fits() {
        // Shrunk from `wrapped_lines_fit_the_width`: U+3000 is two columns,
        // and trim_end() used to count it as the free separator.
        let text = format!("{} ab\u{3000}", "x".repeat(8));
        for l in wrap(vec![Span::raw(text)], 11, vec![], vec![]) {
            assert!(prop::cols(&l) <= 11, "{:?}", plain(&l));
        }
    }

    proptest! {
        #![proptest_config(prop::config())]

        #[test]
        fn render_never_panics(text in prop::text_with(SYNTAX), width in 0usize..120) {
            render(&text, width);
        }

        /// Unindented text wraps to the width (8 at the least), one
        /// character at a time when a word is wider than the line.
        #[test]
        fn wrapped_lines_fit_the_width(text in prop::text(), width in 0usize..80) {
            let text = text.replace(['\n', '\r'], " ");
            for l in wrap(vec![Span::raw(text)], width, vec![], vec![]) {
                // Trailing spaces are blank cells the frame clips; only what shows counts.
                let shown = Line::raw(plain(&l).trim_end_matches(' ').to_string());
                prop_assert!(prop::cols(&shown) <= width.max(8), "{} > {width}: {:?}", prop::cols(&shown), plain(&l));
            }
        }
    }
}
