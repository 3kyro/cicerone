use crate::agent::{self, Agent};
use crate::chat::{Chat, Msg, Role};
use crate::lens::{Lenses, Resolution};
use crate::plan;
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
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

const AUTO: usize = 0;
const HSTEP: usize = 8;
const AGENT: usize = 1;

struct Planning {
    parser: plan::Parser,
    covering: bool,
    status: String,
    error: Option<String>,
    started: Instant,
}

pub struct AgentConfig {
    pub root: PathBuf,
    pub model: Option<String>,
    pub session: Option<String>,
}

pub struct App<'a> {
    ws: &'a Workspace,
    chat: Chat,
    config: Option<AgentConfig>,
    needs_primer: bool,
    resuming: bool,
    turn_text: bool,
    last_question: Option<String>,
    tours: [Tour; 2],
    current: usize,
    agent: Option<Agent>,
    events: Option<Receiver<agent::Event>>,
    planning: Option<Planning>,
    agent_ready: bool,
    cache: Option<PathBuf>,
    notice: Option<String>,
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
    view_width: usize,
    hscroll: usize,
    hmax: usize,
    quit: bool,
}

impl<'a> App<'a> {
    pub fn new(ws: &'a Workspace, auto: Tour, agent: Option<Tour>) -> App<'a> {
        let agent_ready = agent.is_some();
        let current = if agent_ready { AGENT } else { AUTO };
        let agent_tour = agent.unwrap_or_else(|| Tour {
            title: auto.title.clone(),
            intent: String::new(),
            range: None,
            glossary: Vec::new(),
            stages: Vec::new(),
        });
        let mut app = App {
            ws,
            chat: Chat::default(),
            config: None,
            needs_primer: false,
            resuming: false,
            turn_text: false,
            last_question: None,
            tours: [auto, agent_tour],
            current,
            agent: None,
            events: None,
            planning: None,
            agent_ready,
            cache: None,
            notice: None,
            lenses: Lenses::default(),
            steps: Vec::new(),
            pos: 0,
            scroll: 0,
            visited: BTreeSet::from([0]),
            focus: 0,
            overrides: HashMap::new(),
            help: false,
            lens_starts: Vec::new(),
            content_len: 0,
            view_height: 0,
            view_width: 0,
            hscroll: 0,
            hmax: 0,
            quit: false,
        };
        app.refresh_steps();
        app
    }

    pub fn with_config(mut self, config: Option<AgentConfig>, cache: Option<PathBuf>) -> Self {
        self.chat = Chat::load(cache.as_ref().map(|d| d.join("chat.json")).as_deref());
        self.config = config;
        self.cache = cache;
        self
    }

    pub fn planning_with(mut self, agent: Agent, events: Receiver<agent::Event>) -> Self {
        self.agent = Some(agent);
        self.events = Some(events);
        self.planning = Some(Planning {
            parser: plan::Parser::default(),
            covering: false,
            status: "reading the change".into(),
            error: None,
            started: Instant::now(),
        });
        self
    }

    fn chat_path(&self) -> Option<PathBuf> {
        self.cache.as_ref().map(|d| d.join("chat.json"))
    }

    fn start_agent(&mut self) -> Result<()> {
        let Some(cfg) = &self.config else {
            anyhow::bail!("the agent is off (--no-agent)");
        };
        let (session, resume) = match &cfg.session {
            Some(s) => (s.clone(), true),
            None => (agent::new_session_id(), false),
        };
        if !resume && let Some(d) = &self.cache {
            let _ = std::fs::create_dir_all(d);
            let _ = std::fs::write(d.join("session"), &session);
        }
        let log = self.cache.as_ref().map(|d| d.join("agent-chat.log"));
        let (tx, rx) = std::sync::mpsc::channel();
        let a = Agent::start(
            agent::Options {
                cwd: &cfg.root,
                session: &session,
                resume,
                system: plan::SYSTEM,
                model: cfg.model.as_deref(),
                log: log.as_deref(),
                files: self.ws.snapshot.as_deref(),
            },
            tx,
        )?;
        self.agent = Some(a);
        self.events = Some(rx);
        self.resuming = resume;
        self.needs_primer = !resume;
        Ok(())
    }

    fn ask(&mut self) {
        let q = self.chat.input.trim().to_string();
        if q.is_empty() || self.chat.busy {
            return;
        }
        self.chat.input.clear();
        self.chat.scroll = 0;
        let about = self.chat.about.take();
        self.chat.msgs.push(Msg {
            role: Role::User,
            text: q.clone(),
            about: about.as_ref().map(|(label, _)| label.clone()),
        });
        let mut ctx = String::new();
        if let Some(&(si, i)) = self.steps.get(self.pos) {
            let st = &self.tour().stages[si];
            ctx = format!("The reviewer is at stage \"{}\", step \"{}\".", st.title, st.steps[i].title);
        }
        if let Some((_, lens)) = about {
            ctx.push_str(&format!(" They ask about this lens: {lens}."));
        }
        let message = format!("{ctx}\n\nQuestion: {q}");
        if self.planning.is_some() {
            self.chat.pending = Some(message);
            return;
        }
        self.dispatch(message);
    }

    fn dispatch(&mut self, message: String) {
        if self.agent.is_none()
            && let Err(e) = self.start_agent()
        {
            self.chat.push_text(&format!("_{e}_"));
            return;
        }
        let full = if self.needs_primer {
            self.needs_primer = false;
            format!("{}\n\n{message}", plan::primer(self.ws, &self.tours[self.current]))
        } else {
            message.clone()
        };
        self.last_question = Some(message);
        self.turn_text = false;
        if let Some(a) = &mut self.agent
            && a.send(&full).is_ok()
        {
            self.chat.busy = true;
            self.chat.status = "thinking".into();
        } else {
            self.chat.push_text("_could not reach the agent_");
        }
    }

    fn on_chat_event(&mut self, e: agent::Event) {
        match e {
            agent::Event::Text(t) => {
                self.turn_text = true;
                self.chat.push_text(&t);
            }
            agent::Event::Tool(t) => self.chat.status = t,
            agent::Event::TurnDone { ok } => {
                self.chat.busy = false;
                self.resuming = false;
                if !ok {
                    self.chat.push_text("\n\n_the agent reported an error_");
                }
                self.chat.save(self.chat_path().as_deref());
            }
            agent::Event::Exited(err) => {
                self.agent = None;
                self.events = None;
                let retry = self.chat.busy && self.resuming && !self.turn_text;
                self.chat.busy = false;
                if retry {
                    if let Some(c) = &mut self.config {
                        c.session = None;
                    }
                    if let Some(q) = self.last_question.take() {
                        self.dispatch(q);
                    }
                } else {
                    let why = if err.is_empty() { "the agent stopped".to_string() } else { err };
                    self.chat.push_text(&format!("\n\n_{why}_"));
                }
            }
        }
    }

    fn tour(&self) -> &Tour {
        &self.tours[self.current]
    }

    fn refresh_steps(&mut self) {
        self.steps = self.tours[self.current]
            .stages
            .iter()
            .enumerate()
            .flat_map(|(s, st)| (0..st.steps.len()).map(move |i| (s, i)))
            .collect();
        self.pos = self.pos.min(self.steps.len().saturating_sub(1));
    }

    fn switch_tour(&mut self, to: usize) {
        if to == self.current || (to == AGENT && self.tours[AGENT].stages.is_empty()) {
            return;
        }
        self.current = to;
        self.pos = 0;
        self.scroll = 0;
        self.focus = 0;
        self.visited = BTreeSet::from([0]);
        self.overrides.clear();
        self.refresh_steps();
    }

    fn pump(&mut self) {
        let Some(rx) = &self.events else { return };
        let events: Vec<agent::Event> = rx.try_iter().collect();
        for e in events {
            self.on_event(e);
        }
    }

    fn on_event(&mut self, e: agent::Event) {
        let Some(p) = &mut self.planning else {
            self.on_chat_event(e);
            return;
        };
        let parsed = match e {
            agent::Event::Text(t) => p.parser.push(&t),
            agent::Event::Tool(t) => {
                p.status = t;
                Vec::new()
            }
            agent::Event::TurnDone { ok } => {
                let rest = p.parser.finish();
                let covering = p.covering;
                if !ok {
                    p.error = Some("the agent reported an error".into());
                }
                self.apply(rest);
                if ok && !covering && !self.tours[AGENT].stages.is_empty() {
                    let missing = plan::uncovered(&self.tours[AGENT], self.ws);
                    if !missing.is_empty() {
                        let msg = plan::coverage_prompt(self.ws, &missing);
                        if let (Some(a), Some(p)) = (&mut self.agent, &mut self.planning)
                            && a.send(&msg).is_ok()
                        {
                            p.covering = true;
                            p.status = format!("grouping {} more hunks", missing.len());
                            return;
                        }
                    }
                }
                self.finish_planning();
                return;
            }
            agent::Event::Exited(err) => {
                p.error = Some(if err.is_empty() { "the agent stopped".into() } else { err });
                self.agent = None;
                self.events = None;
                self.finish_planning();
                return;
            }
        };
        self.apply(parsed);
    }

    fn apply(&mut self, parsed: Vec<plan::Parsed>) {
        for item in parsed {
            match item {
                plan::Parsed::Header { title, intent, glossary } => {
                    let t = &mut self.tours[AGENT];
                    t.title = title;
                    t.intent = intent;
                    t.glossary = glossary;
                }
                plan::Parsed::Stage(st) => {
                    let first = self.tours[AGENT].stages.is_empty();
                    self.tours[AGENT].stages.push(st);
                    if first && self.current == AUTO && self.visited.len() <= 1 && self.pos == 0 {
                        self.switch_tour(AGENT);
                    } else if first && self.current == AUTO {
                        self.notice = Some("The agent tour has started. Press A to switch.".into());
                    }
                    if self.current == AGENT {
                        self.refresh_steps();
                    }
                }
            }
        }
    }

    fn finish_planning(&mut self) {
        self.finish_plan_tour();
        if let Some(q) = self.chat.pending.take() {
            self.dispatch(q);
        }
    }

    fn finish_plan_tour(&mut self) {
        let Some(p) = self.planning.take() else { return };
        if self.tours[AGENT].stages.is_empty() {
            self.notice = Some(format!(
                "The agent did not return a tour{}. This is the automatic tour.",
                p.error.map(|e| format!(" ({e})")).unwrap_or_default()
            ));
            return;
        }
        plan::complete(&mut self.tours[AGENT], self.ws);
        self.agent_ready = true;
        if self.current == AGENT {
            self.refresh_steps();
        }
        plan::save(&self.tours[AGENT], self.cache.as_deref());
    }

    pub fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            self.pump();
            terminal.draw(|f| self.draw(f))?;
            let tick = if self.planning.is_some() { 80 } else { 250 };
            if event::poll(Duration::from_millis(tick))?
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
                    "right" => KeyCode::Right,
                    "enter" => KeyCode::Enter,
                    "esc" => KeyCode::Esc,
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
            Some(&(s, i)) => &self.tour().stages[s].steps[i].blocks,
            None => &[],
        }
    }

    fn go(&mut self, pos: usize) {
        if pos < self.steps.len() && pos != self.pos {
            self.pos = pos;
            self.scroll = 0;
            self.focus = 0;
            self.hscroll = 0;
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

    fn hscroll_by(&mut self, d: isize) {
        self.hscroll = (self.hscroll as isize + d).clamp(0, self.hmax as isize) as usize;
    }

    fn scroll_by(&mut self, d: isize) {
        let max = self.content_len.saturating_sub(self.view_height / 2);
        self.scroll = (self.scroll as isize + d).clamp(0, max as isize) as usize;
    }

    fn chat_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => self.chat.open = false,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('u') if ctrl => self.chat.input.clear(),
            KeyCode::Char('w') if ctrl => {
                let t = self.chat.input.trim_end().len();
                let cut = self.chat.input[..t].rfind(' ').map_or(0, |i| i + 1);
                self.chat.input.truncate(cut);
            }
            KeyCode::Enter => self.ask(),
            KeyCode::Backspace => {
                self.chat.input.pop();
            }
            KeyCode::PageUp => self.chat.scroll += 5,
            KeyCode::PageDown => self.chat.scroll = self.chat.scroll.saturating_sub(5),
            KeyCode::Char(c) if !ctrl => self.chat.input.push(c),
            _ => {}
        }
    }

    fn open_chat(&mut self, about_lens: bool) {
        self.chat.open = true;
        self.chat.about = None;
        if about_lens && let Some(spec) = self.focused_spec() {
            let (res, diff) = self
                .overrides
                .get(&(self.pos, self.focus))
                .copied()
                .unwrap_or((spec.resolution, None));
            let v = self.lenses.view(self.ws, &spec, res, diff);
            let target = spec
                .symbol
                .clone()
                .or(spec.hunk.clone())
                .unwrap_or_default();
            self.chat.about = Some((
                format!("{} · {}", v.title, v.place),
                format!("{} ({target}) at {}, shown at resolution {}", v.title, v.place, res.label()),
            ));
        }
    }

    fn key(&mut self, k: KeyEvent) {
        if self.help {
            self.help = false;
            return;
        }
        if self.chat.open {
            self.chat_key(k);
            return;
        }
        self.notice = None;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.view_height / 2).max(1) as isize;
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if !ctrl => self.open_chat(false),
            KeyCode::Char('a') => self.open_chat(true),
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('d') if ctrl => self.scroll_by(half),
            KeyCode::Char('u') if ctrl => self.scroll_by(-half),
            KeyCode::Char('n') | KeyCode::Char(' ') => self.go(self.pos + 1),
            KeyCode::Char('p') | KeyCode::Backspace => self.go(self.pos.saturating_sub(1)),
            KeyCode::Char('N') => self.go_stage(true),
            KeyCode::Char('P') => self.go_stage(false),
            KeyCode::Char('l') | KeyCode::Right => self.hscroll_by(HSTEP as isize),
            KeyCode::Char('h') | KeyCode::Left => self.hscroll_by(-(HSTEP as isize)),
            KeyCode::Char('L') => self.hscroll_by((self.view_width / 2) as isize),
            KeyCode::Char('H') => self.hscroll_by(-((self.view_width / 2) as isize)),
            KeyCode::Char('0') => self.hscroll = 0,
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
            KeyCode::Char('A') => self.switch_tour(if self.current == AUTO { AGENT } else { AUTO }),
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
        if self.chat.open {
            let chat_h = (main.height * 45 / 100).max(8);
            let [content, chat] = Layout::vertical([Constraint::Min(4), Constraint::Length(chat_h)]).areas(main);
            self.draw_main(f, content);
            self.draw_chat(f, chat);
        } else {
            self.draw_main(f, main);
        }
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
            Span::styled(self.tour().title.clone(), text().add_modifier(Modifier::BOLD)),
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
        for (si, st) in self.tour().stages.iter().enumerate() {
            let positions: Vec<usize> = (0..self.steps.len()).filter(|&p| self.steps[p].0 == si).collect();
            let all_seen = positions.iter().all(|p| self.visited.contains(p));
            let (mark, style) = if si == cur.0 {
                ("●", Style::default().fg(MAUVE).add_modifier(Modifier::BOLD))
            } else if all_seen {
                ("◉", Style::default().fg(GREEN))
            } else {
                ("○", dim())
            };
            let max = (area.width as usize).saturating_sub(5);
            let title = if st.title.chars().count() > max {
                format!("{}…", st.title.chars().take(max.saturating_sub(1)).collect::<String>())
            } else {
                st.title.clone()
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {mark} "), style),
                Span::styled(title, if si == cur.0 { style } else { text() }),
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
        lines.push(Line::default());
        lines.extend(self.agent_lines(area.width as usize));
        let inner_h = area.height.saturating_sub(2) as usize;
        let off = cur_line.saturating_sub(inner_h.saturating_sub(3));
        let block = Frame::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(SURFACE))
            .padding(Padding::top(1));
        f.render_widget(Paragraph::new(lines).block(block).scroll((off as u16, 0)), area);
    }

    fn agent_lines(&self, w: usize) -> Vec<Line<'static>> {
        let which = if self.current == AGENT { "agent tour" } else { "automatic tour" };
        let mut out = vec![Line::from(vec![
            Span::styled(" ✦ ", Style::default().fg(PEACH)),
            Span::styled(which, Style::default().fg(SUBTEXT)),
        ])];
        if let Some(p) = &self.planning {
            const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let i = (p.started.elapsed().as_millis() / 80) as usize % SPIN.len();
            let status: String = p.status.chars().take(w.saturating_sub(6)).collect();
            out.push(Line::from(vec![
                Span::styled(format!(" {} ", SPIN[i]), Style::default().fg(MAUVE)),
                Span::styled(
                    format!("planning · {}s", p.started.elapsed().as_secs()),
                    Style::default().fg(MAUVE),
                ),
            ]));
            out.push(Line::from(Span::styled(format!("   {status}"), dim())));
            if let Some(e) = &p.error {
                out.push(Line::from(Span::styled(format!("   {e}"), Style::default().fg(RED))));
            }
        } else if self.agent_ready {
            out.push(Line::from(Span::styled("   A switches tours", dim())));
        }
        out
    }

    fn content(&mut self, w: usize) -> Vec<Line<'static>> {
        let Some(&(si, i)) = self.steps.get(self.pos) else {
            return vec![Line::from("This change has nothing to review.")];
        };
        let stage = &self.tour().stages[si];
        let step = &stage.steps[i];
        let mut out: Vec<Line<'static>> = Vec::new();
        if let Some(n) = &self.notice {
            out.push(Line::from(vec![
                Span::styled("✦ ", Style::default().fg(PEACH)),
                Span::styled(n.clone(), Style::default().fg(PEACH)),
            ]));
            out.push(Line::default());
        }
        out.extend([
            Line::from(Span::styled(
                format!("STAGE {} · {}", si + 1, stage.title.to_uppercase()),
                Style::default().fg(OVERLAY).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(step.title.clone(), Style::default().fg(LAVENDER).add_modifier(Modifier::BOLD))),
            Line::default(),
        ]);
        if !step.text.is_empty() {
            out.extend(view::markdown(&step.text, w));
            out.push(Line::default());
        }
        let mut starts = Vec::new();
        let mut li = 0;
        let mut hmax = 0;
        let hs = self.hscroll;
        let blocks = step.blocks.clone();
        for b in &blocks {
            match b {
                Block::Text(t) => out.extend(view::markdown(t, w)),
                Block::Map => out.extend(view::map(self.ws, w)),
                Block::Glossary => out.extend(view::glossary(self.ws, &self.tours[self.current], w)),
                Block::Pattern(id) => out.extend(view::pattern(self.ws, id, w)),
                Block::Mechanical => out.extend(view::mechanical(self.ws, w)),
                Block::Group(g) => {
                    let room = w.saturating_sub(view::GROUP_GUTTER);
                    hmax = hmax.max(view::group_widest(self.ws, g).saturating_sub(room));
                    out.extend(view::group(self.ws, g, w, hs));
                }
                Block::Lens(spec) => {
                    let (res, diff) = self
                        .overrides
                        .get(&(self.pos, li))
                        .copied()
                        .unwrap_or((spec.resolution, None));
                    let v = self.lenses.view(self.ws, spec, res, diff);
                    starts.push(out.len());
                    hmax = hmax.max(view::widest(&v).saturating_sub(w.saturating_sub(view::LENS_GUTTER)));
                    out.extend(view::lens(&v, li == self.focus, w, hs));
                    li += 1;
                }
            }
            out.push(Line::default());
        }
        self.lens_starts = starts;
        self.hmax = hmax;
        out
    }

    fn draw_main(&mut self, f: &mut Canvas, area: Rect) {
        let block = Frame::default().padding(Padding::new(2, 2, 1, 0));
        let inner = block.inner(area);
        self.view_width = inner.width as usize;
        let lines = self.content(inner.width as usize);
        self.hscroll = self.hscroll.min(self.hmax);
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

    fn draw_chat(&self, f: &mut Canvas, area: Rect) {
        let busy = if self.chat.busy { " · answering" } else { "" };
        let block = Frame::default()
            .borders(Borders::TOP)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(MAUVE))
            .title(Span::styled(format!(" ✦ chat{busy} "), Style::default().fg(MAUVE).add_modifier(Modifier::BOLD)))
            .padding(Padding::horizontal(2));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let about_h = u16::from(self.chat.about.is_some());
        let [log, about, input] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(about_h),
            Constraint::Length(1),
        ])
        .areas(inner);
        let lines = self.chat.transcript(log.width as usize);
        let h = log.height as usize;
        let end = lines.len().saturating_sub(self.chat.scroll.min(lines.len().saturating_sub(h)));
        let start = end.saturating_sub(h);
        f.render_widget(Paragraph::new(lines[start..end].to_vec()), log);
        if let Some((label, _)) = &self.chat.about {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("about ", dim()),
                    Span::styled(label.clone(), Style::default().fg(LAVENDER)),
                ])),
                about,
            );
        }
        f.render_widget(Paragraph::new(self.chat.input_line()), input);
    }

    fn draw_status(&self, f: &mut Canvas, area: Rect) {
        if self.chat.open {
            let keys = [("enter", "send"), ("esc", "close"), ("pgup/pgdn", "scroll"), ("ctrl-u", "clear")];
            let mut spans = vec![Span::raw(" ")];
            for (k, what) in keys {
                spans.push(Span::styled(k, Style::default().fg(MAUVE).add_modifier(Modifier::BOLD)));
                spans.push(Span::styled(format!(" {what}   "), dim()));
            }
            f.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(MANTLE)), area);
            return;
        }
        let keys = [
            ("n/p", "step"),
            ("N/P", "stage"),
            ("j/k", "scroll"),
            ("h/l", "sideways"),
            ("tab", "lens"),
            ("+/-", "zoom"),
            ("d", "diff"),
            ("c", "chat"),
            ("a", "ask"),
            ("A", "tour"),
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
        ("n  space", "next step"),
        ("p", "previous step"),
        ("N  P", "next / previous stage"),
        ("h  l  ←  →", "scroll code sideways"),
        ("H  L  0", "half a pane sideways / back to the start"),
        ("j  k  ↓  ↑", "scroll"),
        ("ctrl-d  ctrl-u", "half page"),
        ("g  G", "top / bottom"),
        ("tab  shift-tab", "focus next / previous lens"),
        ("+  -", "zoom the lens: name, signature, skeleton, full"),
        ("d", "show or hide removed lines and markers"),
        ("A", "switch between the agent tour and the automatic tour"),
        ("c", "open the chat with the agent"),
        ("a", "ask about the focused lens"),
        ("esc", "close the chat or this help"),
        ("q", "quit"),
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
