//! Just enough of a Rust lexer for the `arch` gate: identifiers, `::` and
//! single punctuation, each with its line, and nothing from a comment or a
//! string literal. Doc text and strings that mention `crate::x` must never count as
//! an edge, so the gate reads tokens, never lines.
//!
//! `strip_tests` then drops every `#[cfg(test)]` item (and a file under
//! `#![cfg(test)]`), and reports which lines those items covered, so the
//! boundary scan and the line count see only the code that ships.

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Tok {
    Ident(String),
    /// `::`
    PathSep,
    Punct(char),
}

#[derive(Debug, Clone)]
pub(super) struct Token {
    pub(super) tok: Tok,
    pub(super) line: usize,
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The tokens of `src`, lines counted from 1.
pub(super) fn lex(src: &str) -> Vec<Token> {
    let s: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line) = (0, 1);
    let at = |i: usize| s.get(i).copied().unwrap_or('\0');
    while i < s.len() {
        let c = s[i];
        let start_line = line;
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && at(i + 1) == '/' {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && at(i + 1) == '*' {
            let mut depth = 0;
            while i < s.len() {
                if s[i] == '/' && at(i + 1) == '*' {
                    depth += 1;
                    i += 2;
                } else if s[i] == '*' && at(i + 1) == '/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    line += usize::from(s[i] == '\n');
                    i += 1;
                }
            }
        } else if c == '"' {
            i = quoted(&s, i + 1, &mut line);
        } else if c == '\'' {
            // A char literal ('a', '\n', '\u{1F600}') or a lifetime ('a).
            if at(i + 1) == '\\' {
                i += 2;
                while i < s.len() && s[i] != '\'' {
                    i += 1;
                }
                i += 1;
            } else if at(i + 2) == '\'' {
                i += 3;
            } else {
                i += 1;
                while i < s.len() && is_ident(s[i]) {
                    i += 1;
                }
            }
        } else if is_ident(c) {
            let begin = i;
            while i < s.len() && is_ident(s[i]) {
                i += 1;
            }
            let word: String = s[begin..i].iter().collect();
            let raw = matches!(word.as_str(), "r" | "br" | "cr");
            let hashes = s[i..].iter().take_while(|c| **c == '#').count();
            if raw && at(i + hashes) == '"' {
                // r"..", r#".."#: ends at a quote followed by as many hashes.
                let mut j = i + hashes + 1;
                let close = |j: usize| s[j] == '"' && (1..=hashes).all(|k| at(j + k) == '#');
                while j < s.len() && !close(j) {
                    line += usize::from(s[j] == '\n');
                    j += 1;
                }
                i = j + 1 + hashes;
            } else if matches!(word.as_str(), "b" | "c") && at(i) == '"' {
                i = quoted(&s, i + 1, &mut line);
            } else if word == "b" && at(i) == '\'' {
                i += 1;
                while i < s.len() && s[i] != '\'' {
                    i += if s[i] == '\\' { 2 } else { 1 };
                }
                i += 1;
            } else if word == "r" && hashes == 1 && is_ident(at(i + 1)) {
                // A raw identifier, r#type: the word after the hash.
                i += 1;
            } else if !c.is_ascii_digit() {
                out.push(Token { tok: Tok::Ident(word), line: start_line });
            }
        } else if c == ':' && at(i + 1) == ':' {
            out.push(Token { tok: Tok::PathSep, line });
            i += 2;
        } else {
            out.push(Token { tok: Tok::Punct(c), line });
            i += 1;
        }
    }
    out
}

/// The index after the closing quote of a `"`-delimited body starting at `i`.
fn quoted(s: &[char], mut i: usize, line: &mut usize) -> usize {
    while i < s.len() && s[i] != '"' {
        *line += usize::from(s[i] == '\n');
        if s[i] == '\\' {
            i += 1;
            *line += usize::from(s.get(i) == Some(&'\n'));
        }
        i += 1;
    }
    i + 1
}

fn punct(t: &[Token], i: usize, c: char) -> bool {
    t.get(i).is_some_and(|t| t.tok == Tok::Punct(c))
}

/// Index after the bracket group that opens at `i` (`(`, `[` or `{`).
fn close(t: &[Token], i: usize) -> usize {
    let mut depth = 0i32;
    for (j, tok) in t.iter().enumerate().skip(i) {
        match tok.tok {
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => {
                depth -= 1;
                if depth == 0 {
                    return j + 1;
                }
            }
            _ => {}
        }
    }
    t.len()
}

/// If an attribute `#[cfg(test)]` (or `#![...]` when `inner`) starts at `i`,
/// the index after it.
fn cfg_test(t: &[Token], i: usize, inner: bool) -> Option<usize> {
    let ident = |j: usize, w: &str| matches!(&t.get(j)?.tok, Tok::Ident(x) if x == w).then_some(());
    let mut j = i + 1;
    if inner {
        punct(t, j, '!').then_some(())?;
        j += 1;
    }
    (punct(t, i, '#') && punct(t, j, '[')).then_some(())?;
    ident(j + 1, "cfg")?;
    (punct(t, j + 2, '(') && punct(t, j + 4, ')') && punct(t, j + 5, ']')).then_some(())?;
    ident(j + 3, "test")?;
    Some(j + 6)
}

/// The tokens outside `#[cfg(test)]` items, and per line (index 0 is line 1)
/// whether a test item covered it. A file whose first attribute is
/// `#![cfg(test)]` is all test.
pub(super) fn strip_tests(t: Vec<Token>, lines: usize) -> (Vec<Token>, Vec<bool>) {
    let mut test = vec![false; lines];
    if cfg_test(&t, 0, true).is_some() {
        return (Vec::new(), vec![true; lines]);
    }
    let mut keep = Vec::with_capacity(t.len());
    let mut i = 0;
    while i < t.len() {
        let Some(mut j) = cfg_test(&t, i, false) else {
            keep.push(t[i].clone());
            i += 1;
            continue;
        };
        // Further attributes, then the item: up to a `;` or through a `{}` body.
        while punct(&t, j, '#') && punct(&t, j + 1, '[') {
            j = close(&t, j + 1);
        }
        while j < t.len() {
            match t[j].tok {
                Tok::Punct(';') => {
                    j += 1;
                    break;
                }
                Tok::Punct('{') => {
                    j = close(&t, j);
                    break;
                }
                Tok::Punct('(' | '[') => j = close(&t, j),
                _ => j += 1,
            }
        }
        let last = t.get(j.saturating_sub(1)).map_or(lines, |x| x.line);
        for l in t[i].line..=last {
            if let Some(slot) = test.get_mut(l - 1) {
                *slot = true;
            }
        }
        i = j;
    }
    (keep, test)
}
