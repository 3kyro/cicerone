use crate::theme::*;
use crate::view;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde::{Deserialize, Serialize};

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

    pub fn input_line(&self) -> Line<'static> {
        let mut spans = vec![Span::styled("› ", Style::default().fg(MAUVE).add_modifier(Modifier::BOLD))];
        if self.input.is_empty() {
            spans.push(Span::styled("▍", Style::default().fg(MAUVE)));
            spans.push(Span::styled(
                if self.about.is_some() { " ask about this lens" } else { " ask about the change" },
                dim(),
            ));
        } else {
            spans.push(Span::styled(self.input.clone(), text()));
            spans.push(Span::styled("▍", Style::default().fg(MAUVE)));
        }
        Line::from(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
