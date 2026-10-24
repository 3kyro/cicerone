use crate::lens::{Lenses, Resolution};
use crate::manifest::Workspace;
use crate::theme::*;
use crate::tour::{Block, Tour};
use crate::view;
use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as Frame, BorderType, Borders, Clear, Padding, Paragraph};
use ratatui::{DefaultTerminal, Frame as Canvas};
use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

pub struct App<'a> {
    ws: &'a Workspace,
    tour: Tour,
    lenses: Lenses,
    steps: Vec<(usize, usize)>,
    pos: usize,
    scroll: usize,
    visited: BTreeSet<usize>,
    focus: usize,
    overrides: HashMap<(usize, usize), (Resolution, Option<bool>)>,
    help: bool,
    lens_starts: Vec<usize>,
    content_len: usize,
    view_height: usize,
    quit: bool,
}

impl<'a> App<'a> {
    pub fn new(ws: &'a Workspace, tour: Tour) -> App<'a> {
        let steps: Vec<(usize, usize)> = tour
            .stages
            .iter()
            .enumerate()
            .flat_map(|(s, st)| (0..st.steps.len()).map(move |i| (s, i)))
            .collect();
        App {
            ws,
            tour,
            lenses: Lenses::default(),
            steps,
            pos: 0,
            scroll: 0,
            visited: BTreeSet::from([0]),
            focus: 0,
            overrides: HashMap::new(),
            help: false,
            lens_starts: Vec::new(),
            content_len: 0,
            view_height: 0,
            quit: false,
        }
    }

    pub fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(250))?
                && let Event::Key(k) = event::read()?
                && k.kind == KeyEventKind::Press
            {
                self.key(k);
            }
        }
        Ok(())
    }

    /// The screen after `keys`, as plain text. Each key is one char, except
    /// `<tab>`, `<down>`, and the like.
    pub fn snapshot(mut self, keys: &str, w: u16, h: u16) -> Result<String> {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h))?;
        term.draw(|f| self.draw(f))?;
        let mut rest = keys;
        while !rest.is_empty() {
            let (code, n) = if let Some(end) = rest.strip_prefix('<').and_then(|r| r.find('>')) {
                let name = &rest[1..end + 1];
                let code = match name {
                    "tab" => KeyCode::Tab,
                    "down" => KeyCode::Down,
                    "up" => KeyCode::Up,
                    "pgdn" => KeyCode::PageDown,
                    _ => KeyCode::Null,
                };
                (code, end + 2)
            } else {
                let c = rest.chars().next().unwrap();
                (KeyCode::Char(c), c.len_utf8())
            };
            self.key(KeyEvent::new(code, KeyModifiers::NONE));
            term.draw(|f| self.draw(f))?;
            rest = &rest[n..];
        }
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            let line: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect();
            out.push_str(line.trim_end());
            out.push('\n');
        }
        Ok(out)
    }

    fn lens_count(&self) -> usize {
        self.step_blocks().iter().filter(|b| matches!(b, Block::Lens(_))).count()
    }

    fn step_blocks(&self) -> &[Block] {
        match self.steps.get(self.pos) {
            Some(&(s, i)) => &self.tour.stages[s].steps[i].blocks,
            None => &[],
        }
    }

    fn go(&mut self, pos: usize) {
        if pos < self.steps.len() && pos != self.pos {
            self.pos = pos;
            self.scroll = 0;
            self.focus = 0;
            self.visited.insert(pos);
        }
    }

    fn go_stage(&mut self, forward: bool) {
        let stage = self.steps.get(self.pos).map_or(0, |s| s.0);
        let target = if forward {
            self.steps.iter().position(|s| s.0 > stage)
        } else {
            let prev = if self.steps[self.pos].1 > 0 { stage } else { stage.saturating_sub(1) };
            self.steps.iter().position(|s| s.0 == prev)
        };
        if let Some(t) = target {
            self.go(t);
        }
    }

    fn zoom(&mut self, deeper: bool) {
        let Some(spec) = self.focused_spec() else { return };
        let available = Lenses::available(self.ws, &spec);
        let key = (self.pos, self.focus);
        let (cur, diff) = self.overrides.get(&key).copied().unwrap_or((spec.resolution, None));
        let i = available.iter().position(|r| *r == cur).unwrap_or(available.len() - 1);
        let j = if deeper { (i + 1).min(available.len() - 1) } else { i.saturating_sub(1) };
        self.overrides.insert(key, (available[j], diff));
    }

    fn toggle_diff(&mut self) {
        let Some(spec) = self.focused_spec() else { return };
        let key = (self.pos, self.focus);
        let (res, diff) = self.overrides.get(&key).copied().unwrap_or((spec.resolution, None));
        let current = self.lenses.view(self.ws, &spec, res, diff).diff;
        self.overrides.insert(key, (res, Some(!current)));
    }

    fn focused_spec(&self) -> Option<crate::lens::LensSpec> {
        self.step_blocks()
            .iter()
            .filter_map(|b| match b {
                Block::Lens(l) => Some(l.clone()),
                _ => None,
            })
            .nth(self.focus)
    }

    fn scroll_by(&mut self, d: isize) {
        let max = self.content_len.saturating_sub(self.view_height / 2);
        self.scroll = (self.scroll as isize + d).clamp(0, max as isize) as usize;
    }

    fn key(&mut self, k: KeyEvent) {
        if self.help {
            self.help = false;
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.view_height / 2).max(1) as isize;
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('d') if ctrl => self.scroll_by(half),
            KeyCode::Char('u') if ctrl => self.scroll_by(-half),
            KeyCode::Char('n') | KeyCode::Char(' ') | KeyCode::Char('l') | KeyCode::Right => self.go(self.pos + 1),
            KeyCode::Char('p') | KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => {
                self.go(self.pos.saturating_sub(1))
            }
            KeyCode::Char('N') | KeyCode::Char('L') => self.go_stage(true),
            KeyCode::Char('P') | KeyCode::Char('H') => self.go_stage(false),
            KeyCode::Char('j') | KeyCode::Down => self.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_by(-1),
            KeyCode::PageDown => self.scroll_by(half * 2),
            KeyCode::PageUp => self.scroll_by(-half * 2),
            KeyCode::Char('g') | KeyCode::Home => self.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.scroll_by(self.content_len as isize),
            KeyCode::Tab | KeyCode::BackTab => {
                let n = self.lens_count();
                if n > 0 {
                    self.focus = if k.code == KeyCode::Tab { (self.focus + 1) % n } else { (self.focus + n - 1) % n };
                    if let Some(&s) = self.lens_starts.get(self.focus) {
                        self.scroll = s.saturating_sub(1);
                    }
                }
            }
            KeyCode::Char('+') | KeyCode::Char('=') => self.zoom(true),
            KeyCode::Char('-') | KeyCode::Char('_') => self.zoom(false),
            KeyCode::Char('d') => self.toggle_diff(),
            KeyCode::Char('?') => self.help = true,
            _ => {}
        }
    }

    fn draw(&mut self, f: &mut Canvas) {
        let area = f.area();
        f.render_widget(Paragraph::new("").style(Style::default().bg(BASE)), area);
        let [header, body, status] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)]).areas(area);
        let rail_w = if area.width >= 100 { 32 } else { 0 };
        let [rail, main] = Layout::horizontal([Constraint::Length(rail_w), Constraint::Min(20)]).areas(body);
        self.draw_header(f, header);
        if rail_w > 0 {
            self.draw_rail(f, rail);
        }
        self.draw_main(f, main);
        self.draw_status(f, status);
        if self.help {
            draw_help(f, area);
        }
    }

    fn draw_header(&self, f: &mut Canvas, area: Rect) {
        let total = self.steps.len().max(1);
        let bar_w = 20usize;
        let done = self.visited.len() * bar_w / total;
        let right = vec![
            Span::styled(format!("step {}/{}  ", self.pos + 1, total), dim()),
            Span::styled("━".repeat(done), Style::default().fg(MAUVE)),
            Span::styled("─".repeat(bar_w - done), Style::default().fg(SURFACE)),
            Span::raw(" "),
        ];
        let left = vec![
            Span::styled(" cicerone ", Style::default().fg(BASE).bg(MAUVE).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
            Span::styled(self.tour.title.clone(), text().add_modifier(Modifier::BOLD)),
        ];
        let rw: usize = right.iter().map(Span::width).sum();
        f.render_widget(Paragraph::new(Line::from(left)).style(Style::default().bg(MANTLE)), area);
        let r = Rect {
            x: area.x + area.width.saturating_sub(rw as u16),
            width: (rw as u16).min(area.width),
            ..area
        };
        f.render_widget(Paragraph::new(Line::from(right)).style(Style::default().bg(MANTLE)), r);
    }

    fn draw_rail(&self, f: &mut Canvas, area: Rect) {
        let cur = self.steps.get(self.pos).copied().unwrap_or((0, 0));
        let mut lines: Vec<Line> = Vec::new();
        let mut cur_line = 0;
        for (si, st) in self.tour.stages.iter().enumerate() {
            let positions: Vec<usize> = (0..self.steps.len()).filter(|&p| self.steps[p].0 == si).collect();
            let all_seen = positions.iter().all(|p| self.visited.contains(p));
            let (mark, style) = if si == cur.0 {
                ("●", Style::default().fg(MAUVE).add_modifier(Modifier::BOLD))
            } else if all_seen {
                ("◉", Style::default().fg(GREEN))
            } else {
                ("○", dim())
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {mark} "), style),
                Span::styled(st.title.clone(), if si == cur.0 { style } else { text() }),
            ]));
            if si == cur.0 {
                for (i, step) in st.steps.iter().enumerate() {
                    let p = positions[i];
                    let (m, s) = if p == self.pos {
                        cur_line = lines.len();
                        ("▸", Style::default().fg(LAVENDER).add_modifier(Modifier::BOLD))
                    } else if self.visited.contains(&p) {
                        ("✓", Style::default().fg(GREEN))
                    } else {
                        ("·", dim())
                    };
                    let title: String = step.title.chars().take(area.width.saturating_sub(8) as usize).collect();
                    lines.push(Line::from(vec![
                        Span::raw("    "),
                        Span::styled(format!("{m} "), s),
                        Span::styled(title, if p == self.pos { s } else { Style::default().fg(SUBTEXT) }),
                    ]));
                }
            }
        }
        let inner_h = area.height.saturating_sub(2) as usize;
        let off = cur_line.saturating_sub(inner_h.saturating_sub(3));
        let block = Frame::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(SURFACE))
            .padding(Padding::top(1));
        f.render_widget(Paragraph::new(lines).block(block).scroll((off as u16, 0)), area);
    }

    fn content(&mut self, w: usize) -> Vec<Line<'static>> {
        let Some(&(si, i)) = self.steps.get(self.pos) else {
            return vec![Line::from("This change has nothing to review.")];
        };
        let stage = &self.tour.stages[si];
        let step = &stage.steps[i];
        let mut out: Vec<Line<'static>> = vec![
            Line::from(Span::styled(
                format!("STAGE {} · {}", si + 1, stage.title.to_uppercase()),
                Style::default().fg(OVERLAY).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(step.title.clone(), Style::default().fg(LAVENDER).add_modifier(Modifier::BOLD))),
            Line::default(),
        ];
        if !step.text.is_empty() {
            out.extend(view::markdown(&step.text, w));
            out.push(Line::default());
        }
        let mut starts = Vec::new();
        let mut li = 0;
        let blocks = step.blocks.clone();
        for b in &blocks {
            match b {
                Block::Text(t) => out.extend(view::markdown(t, w)),
                Block::Map => out.extend(view::map(self.ws, w)),
                Block::Glossary => out.extend(view::glossary(self.ws, &self.tour, w)),
                Block::Pattern(id) => out.extend(view::pattern(self.ws, id, w)),
                Block::Mechanical => out.extend(view::mechanical(self.ws, w)),
                Block::Lens(spec) => {
                    let (res, diff) = self
                        .overrides
                        .get(&(self.pos, li))
                        .copied()
                        .unwrap_or((spec.resolution, None));
                    let v = self.lenses.view(self.ws, spec, res, diff);
                    starts.push(out.len());
                    out.extend(view::lens(&v, li == self.focus, w));
                    li += 1;
                }
            }
            out.push(Line::default());
        }
        self.lens_starts = starts;
        out
    }

    fn draw_main(&mut self, f: &mut Canvas, area: Rect) {
        let block = Frame::default().padding(Padding::new(2, 2, 1, 0));
        let inner = block.inner(area);
        let lines = self.content(inner.width as usize);
        self.content_len = lines.len();
        self.view_height = inner.height as usize;
        self.scroll = self.scroll.min(self.content_len.saturating_sub(1));
        f.render_widget(Paragraph::new(lines).block(block).scroll((self.scroll as u16, 0)), area);
        if self.content_len > self.view_height {
            let pct = (self.scroll * 100) / self.content_len.saturating_sub(self.view_height).max(1);
            let tag = format!(" {}% ", pct.min(100));
            let r = Rect {
                x: area.x + area.width.saturating_sub(tag.len() as u16 + 1),
                y: area.y + area.height.saturating_sub(1),
                width: tag.len() as u16,
                height: 1,
            };
            f.render_widget(Paragraph::new(Span::styled(tag, dim())), r);
        }
    }

    fn draw_status(&self, f: &mut Canvas, area: Rect) {
        let keys = [
            ("n/p", "step"),
            ("N/P", "stage"),
            ("j/k", "scroll"),
            ("tab", "lens"),
            ("+/-", "zoom"),
            ("d", "diff"),
            ("?", "help"),
            ("q", "quit"),
        ];
        let mut spans = vec![Span::raw(" ")];
        for (k, what) in keys {
            spans.push(Span::styled(k, Style::default().fg(MAUVE).add_modifier(Modifier::BOLD)));
            spans.push(Span::styled(format!(" {what}   "), dim()));
        }
        f.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(MANTLE)), area);
    }
}

fn draw_help(f: &mut Canvas, area: Rect) {
    let rows = [
        ("n  space  l  →", "next step"),
        ("p  h  ←", "previous step"),
        ("N  P", "next / previous stage"),
        ("j  k  ↓  ↑", "scroll"),
        ("ctrl-d  ctrl-u", "half page"),
        ("g  G", "top / bottom"),
        ("tab  shift-tab", "focus next / previous lens"),
        ("+  -", "zoom the lens: name, signature, skeleton, full"),
        ("d", "show or hide removed lines and markers"),
        ("q  esc", "quit"),
    ];
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("{k:>16}  "), Style::default().fg(MAUVE).add_modifier(Modifier::BOLD)),
                Span::styled(v.to_string(), text()),
            ])
        })
        .collect();
    let w = 70.min(area.width);
    let h = (rows.len() as u16 + 4).min(area.height);
    let r = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, r);
    let block = Frame::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(MAUVE))
        .title(Span::styled(" keys ", Style::default().fg(MAUVE).add_modifier(Modifier::BOLD)))
        .padding(Padding::uniform(1))
        .style(Style::default().bg(MANTLE));
    f.render_widget(Paragraph::new(lines).block(block), r);
}
