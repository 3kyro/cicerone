use crate::theme::*;
use crate::view;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde::{Deserialize, Serialize};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Agent,
    Event,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Msg {
    pub role: Role,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
}

/// Takes the finding updates out of streamed answer text. A line that may
/// still turn into one is held back until it ends.
#[derive(Default)]
pub struct Filter {
    held: String,
}

const MARK: &str = "{\"finding";

impl Filter {
    pub fn push(&mut self, text: &str) -> (String, Vec<String>) {
        self.held.push_str(text);
        let mut shown = String::new();
        let mut items = Vec::new();
        while let Some(i) = self.held.find('\n') {
            let line: String = self.held.drain(..=i).collect();
            if line.trim_start().starts_with(MARK) {
                items.push(line.trim().to_string());
            } else {
                shown.push_str(&line);
            }
        }
        let rest = self.held.trim_start();
        let held_back = rest.starts_with('{') && (MARK.starts_with(rest) || rest.starts_with(MARK));
        if !held_back {
            shown.push_str(&std::mem::take(&mut self.held));
        }
        (shown, items)
    }

    pub fn finish(&mut self) -> (String, Vec<String>) {
        let rest = std::mem::take(&mut self.held);
        if rest.trim_start().starts_with(MARK) {
            (String::new(), vec![rest.trim().to_string()])
        } else {
            (rest, Vec::new())
        }
    }
}

#[derive(Default)]
pub struct Chat {
    pub open: bool,
    pub input: String,
    pub msgs: Vec<Msg>,
    pub busy: bool,
    pub status: String,
    /// The question that waits for the planning turn to end.
    pub pending: Option<String>,
    /// Lines scrolled up from the bottom of the transcript.
    pub scroll: usize,
    pub about: Option<(String, String)>,
    /// A short confirmation for the title bar, cleared on the next key.
    pub flash: Option<String>,
}

impl Chat {
    pub fn load(path: Option<&std::path::Path>) -> Chat {
        let msgs = path
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        Chat {
            msgs,
            ..Chat::default()
        }
    }

    pub fn save(&self, path: Option<&std::path::Path>) {
        if let Some(p) = path
            && let Ok(j) = serde_json::to_string_pretty(&self.msgs)
        {
            let _ = std::fs::write(p, j);
        }
    }

    pub fn push_event(&mut self, t: String) {
        self.msgs.push(Msg {
            role: Role::Event,
            text: t,
            about: None,
        });
    }

    pub fn push_text(&mut self, t: &str) {
        match self.msgs.last_mut() {
            Some(m) if m.role == Role::Agent => m.text.push_str(t),
            _ => self.msgs.push(Msg {
                role: Role::Agent,
                text: t.to_string(),
                about: None,
            }),
        }
        self.status.clear();
    }

    pub fn transcript(&self, w: usize) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for m in &self.msgs {
            match m.role {
                Role::User => {
                    if let Some(a) = &m.about {
                        out.push(Line::from(Span::styled(format!("  about {a}"), dim())));
                    }
                    for (i, l) in view::markdown(&m.text, w.saturating_sub(2)).into_iter().enumerate() {
                        let mut spans = vec![Span::styled(
                            if i == 0 { "› " } else { "  " },
                            Style::default().fg(MAUVE).add_modifier(Modifier::BOLD),
                        )];
                        spans.extend(l.spans.into_iter().map(|s| {
                            let st = s.style.fg(LAVENDER);
                            s.style(st)
                        }));
                        out.push(Line::from(spans));
                    }
                }
                Role::Event => {
                    out.push(Line::from(Span::styled(
                        format!("↻ {}", m.text),
                        Style::default().fg(PEACH).add_modifier(Modifier::ITALIC),
                    )));
                }
                Role::Agent => {
                    for (i, l) in view::markdown(&m.text, w.saturating_sub(2)).into_iter().enumerate() {
                        let mut spans = vec![Span::styled(
                            if i == 0 { "✦ " } else { "  " },
                            Style::default().fg(PEACH),
                        )];
                        spans.extend(l.spans);
                        out.push(Line::from(spans));
                    }
                }
            }
            out.push(Line::default());
        }
        if self.busy {
            let status = if self.status.is_empty() { "thinking" } else { &self.status };
            out.push(Line::from(Span::styled(format!("  · {status}"), dim().add_modifier(Modifier::ITALIC))));
        }
        if let Some(p) = &self.pending {
            out.push(Line::from(Span::styled(
                format!("  · waits for the planning to end: {p}"),
                dim().add_modifier(Modifier::ITALIC),
            )));
        }
        out
    }

    /// The input wrapped at `w` columns, the cursor after the last
    /// character.
    pub fn input_lines(&self, w: usize) -> Vec<Line<'static>> {
        let prompt = Style::default().fg(MAUVE).add_modifier(Modifier::BOLD);
        let cursor = Span::styled("▍", Style::default().fg(MAUVE));
        if self.input.is_empty() {
            let hint = if self.about.is_some() { " ask about this lens" } else { " ask about the change" };
            return vec![Line::from(vec![Span::styled("› ", prompt), cursor, Span::styled(hint, dim())])];
        }
        let room = w.saturating_sub(3).max(1);
        let mut rows: Vec<(String, usize)> = vec![(String::new(), 0)];
        for piece in self.input.split_inclusive(' ') {
            let pw = piece.width();
            let used = rows.last().map_or(0, |r| r.1);
            if used + pw <= room {
                let row = rows.last_mut().unwrap();
                row.0.push_str(piece);
                row.1 += pw;
            } else if piece.trim().is_empty() {
                rows.push((String::new(), 0));
            } else if used > 0 && pw <= room {
                rows.push((piece.to_string(), pw));
            } else {
                for c in piece.chars() {
                    let cw = c.width().unwrap_or(0);
                    let row = rows.last_mut().unwrap();
                    if row.1 + cw > room && row.1 > 0 {
                        rows.push((c.to_string(), cw));
                    } else {
                        row.0.push(c);
                        row.1 += cw;
                    }
                }
            }
        }
        if rows.len() > 1 && rows.last().is_some_and(|r| r.0.is_empty()) {
            rows.pop();
        }
        let rows: Vec<String> = rows.into_iter().map(|r| r.0).collect();
        let last = rows.len() - 1;
        rows.into_iter()
            .enumerate()
            .map(|(i, row)| {
                let mut spans = vec![
                    Span::styled(if i == 0 { "› " } else { "  " }, prompt),
                    Span::styled(row, text()),
                ];
                if i == last {
                    spans.push(cursor.clone());
                }
                Line::from(spans)
            })
            .collect()
    }
}

/// Puts `text` on the system clipboard through the terminal (OSC 52).
pub fn copy_to_clipboard(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout();
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_wraps_and_keeps_the_cursor_at_the_end() {
        let render = |input: &str, w: usize| -> Vec<String> {
            let chat = Chat {
                input: input.into(),
                ..Chat::default()
            };
            chat.input_lines(w)
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect()
        };
        assert_eq!(render("abcdefgh", 6), vec!["› abc", "  def", "  gh▍"]);
        assert_eq!(render("why does it log", 10), vec!["› why ", "  does ", "  it log▍"]);
        assert_eq!(render("abcdef    ", 10), vec!["› abcdef ", "    ▍"], "spaces past the edge wrap");
        assert_eq!(render("abcdef    x", 10), vec!["› abcdef ", "    x▍"]);
        assert_eq!(render("日本語テキスト", 10), vec!["› 日本語", "  テキス", "  ト▍"], "wide characters take two columns");
    }

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("ü✦".as_bytes()), "w7zinKY=");
    }

    #[test]
    fn filter_takes_out_update_lines_across_chunks() {
        let mut f = Filter::default();
        let (a, i1) = f.push("I agree.\n{\"finding_up");
        let (b, i2) = f.push("date\":{\"id\":\"F1\"}}\nSo it is a nit.");
        let (c, i3) = f.finish();
        assert_eq!(format!("{a}{b}{c}"), "I agree.\nSo it is a nit.");
        assert!(i1.is_empty());
        assert_eq!(i2, vec!["{\"finding_update\":{\"id\":\"F1\"}}"]);
        assert!(i3.is_empty());
        let (d, _) = f.push("{x} is code\n");
        assert_eq!(d, "{x} is code\n");
    }
}
