use crate::agent::{self, Agent};
use crate::chat::{self, Chat, Msg, Role};
use crate::lens::{Lenses, Resolution};
use crate::classes::Classes;
use crate::plan::{self, Phase};
use crate::review;
use crate::{github, report, session, split};
use crate::manifest::Workspace;
use crate::theme::*;
use crate::tour::{Block, Tour};
use crate::view;
use anyhow::Result;
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
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
const MAX_INPUT_LINES: usize = 6;
const AGENT: usize = 1;

struct Planning {
    parser: plan::Parser,
    phase: Phase,
    status: String,
    error: Option<String>,
    started: Instant,
    /// A cached tour that has no review yet: stop after the review turn.
    review_only: bool,
    helpers: Option<Helpers>,
}

struct Helpers {
    parts: Vec<Vec<String>>,
    rx: Receiver<split::Msg>,
    done: usize,
    findings: Vec<String>,
    failed: Vec<String>,
}

struct ReportJob {
    text: String,
    status: String,
    started: Instant,
}

pub struct AgentConfig {
    pub root: PathBuf,
    pub model: Option<String>,
    pub session: Option<String>,
}

pub struct App<'a> {
    ws: &'a Workspace,
    findings: Vec<review::Finding>,
    classes: Option<Classes>,
    step_findings: Vec<Vec<usize>>,
    chat_filter: chat::Filter,
    report: Option<ReportJob>,
    root: PathBuf,
    pr: Option<u64>,
    compose: bool,
    scratch: Option<PathBuf>,
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
    chat_area: Rect,
    main_area: Rect,
    chat_page: usize,
    chat_max: usize,
    mouse: bool,
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
            findings: Vec::new(),
            classes: None,
            step_findings: Vec::new(),
            chat_filter: chat::Filter::default(),
            report: None,
            root: PathBuf::new(),
            pr: None,
            compose: false,
            scratch: None,
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
            chat_area: Rect::default(),
            main_area: Rect::default(),
            chat_page: 1,
            chat_max: 0,
            mouse: true,
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

    pub fn planning_with(
        mut self,
        agent: Agent,
        events: Receiver<agent::Event>,
        helpers: Option<(Vec<Vec<String>>, Receiver<split::Msg>)>,
    ) -> Self {
        self.agent = Some(agent);
        self.events = Some(events);
        self.classes = Some(Classes::default());
        self.planning = Some(Planning {
            parser: plan::Parser::default(),
            phase: if helpers.is_some() { Phase::Split } else { Phase::Review },
            status: "reading the change".into(),
            error: None,
            started: Instant::now(),
            review_only: false,
            helpers: helpers.map(|(parts, rx)| Helpers {
                parts,
                rx,
                done: 0,
                findings: Vec::new(),
                failed: Vec::new(),
            }),
        });
        self
    }

    /// Collects the helpers' reviews; when the last part is done, the main
    /// session checks and merges them.
    fn pump_helpers(&mut self) {
        let Some(p) = &mut self.planning else { return };
        let Some(h) = &mut p.helpers else { return };
        for m in h.rx.try_iter() {
            match m {
                split::Msg::Finding(f) => h.findings.push(f),
                split::Msg::Tool(t) => p.status = t,
                split::Msg::Done { part, ok } => {
                    h.done += 1;
                    if !ok {
                        h.failed.push(format!("part {}: {}", part + 1, h.parts[part].join(", ")));
                    }
                }
            }
        }
        if h.done < h.parts.len() {
            return;
        }
        let h = p.helpers.take().unwrap();
        let msg = split::merge_prompt(self.ws, h.parts.len(), &h.findings, &h.failed);
        if !self.turn(&msg, Phase::Review, Phase::Review.status().into()) {
            self.end_planning();
        }
    }

    pub fn with_classes(mut self, classes: Option<Classes>) -> Self {
        self.classes = classes;
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
        let tools = self.cache.as_ref().map(|d| d.join("tools.log"));
        let (tx, rx) = std::sync::mpsc::channel();
        let a = Agent::start(
            agent::Options {
                cwd: &cfg.root,
                session: &session,
                resume,
                system: plan::SYSTEM,
                model: cfg.model.as_deref(),
                log: log.as_deref(),
                tools: tools.as_deref(),
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
            ctx.push_str(&format!(" They ask about {lens}."));
        }
        let open: Vec<String> = self
            .findings
            .iter()
            .filter(|f| !f.withdrawn)
            .map(|f| format!("{} [{}] {}", f.id, f.severity.label(), f.title))
            .collect();
        if !open.is_empty() {
            ctx.push_str(&format!("\nOpen review findings: {}.\n{}", open.join("; "), review::UPDATE_RULES));
        }
        let message = format!("{ctx}\n\nQuestion: {q}");
        if self.planning.is_some() {
            self.chat.pending = Some(message);
            return;
        }
        self.dispatch(message);
    }

    fn with_primer(&mut self, message: String) -> String {
        if !self.needs_primer {
            return message;
        }
        self.needs_primer = false;
        format!("{}\n\n{message}", plan::primer(self.ws, &self.tours[self.current]))
    }

    fn dispatch(&mut self, message: String) {
        if self.agent.is_none()
            && let Err(e) = self.start_agent()
        {
            self.chat.push_text(&format!("_{e}_"));
            return;
        }
        let full = self.with_primer(message.clone());
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

    fn chat_output(&mut self, shown: String, items: Vec<String>) {
        if !shown.is_empty() {
            self.chat.push_text(&shown);
        }
        let mut changed = false;
        for line in items {
            let event = match plan::parse_line(&line) {
                Some(plan::Parsed::Update(u)) => {
                    let id = u.id.clone();
                    let reason = u.reason.clone();
                    let ws = self.ws;
                    self.findings
                        .iter_mut()
                        .find(|f| f.id == id)
                        .and_then(|f| {
                            let before = f.lines;
                            let what = f.revise(u)?;
                            if !review::clamp(f, ws) {
                                f.lines = before;
                            }
                            Some(what)
                        })
                        .map(|what| format!("{id} {what}: {reason}"))
                }
                Some(plan::Parsed::Withdraw(w)) => self
                    .findings
                    .iter_mut()
                    .find(|f| f.id == w.id)
                    .and_then(|f| f.withdraw(&w.reason).then(|| format!("{} withdrawn: {}", w.id, w.reason))),
                Some(plan::Parsed::Finding(f)) => review::accept(f, self.ws, self.findings.len() + 1).map(|f| {
                    let e = format!("{} new {}: {}", f.id, f.severity.label(), f.title);
                    self.findings.push(f);
                    e
                }),
                None => Some(format!("could not read a finding change: {}", crate::clean(&line))),
                _ => None,
            };
            if let Some(e) = event {
                self.chat.push_event(e);
                changed = true;
            }
        }
        if changed {
            self.sync_findings();
        }
    }

    fn start_report(&mut self) {
        if self.report.is_some() {
            return;
        }
        if self.planning.is_some() || self.chat.busy {
            self.notice = Some("The agent is busy. Press E again when it is done.".into());
            return;
        }
        let Some(root) = self.config.as_ref().map(|c| c.root.clone()) else {
            self.notice = Some("The agent is off (--no-agent), so it cannot write a report.".into());
            return;
        };
        if self.agent.is_none()
            && let Err(e) = self.start_agent()
        {
            self.notice = Some(format!("Could not start the agent: {e}"));
            return;
        }
        let range = format!("{}..{}", self.ws.manifest.base, self.ws.manifest.head);
        let changes = self.classes.as_ref().map(|c| c.changes.clone()).unwrap_or_default();
        let msg = self.with_primer(report::prompt(&self.findings, &changes, &root, &range));
        if self.agent.as_mut().is_some_and(|a| a.send(&msg).is_ok()) {
            self.report = Some(ReportJob {
                text: String::new(),
                status: "writing the report".into(),
                started: Instant::now(),
            });
        } else {
            self.notice = Some("Could not reach the agent.".into());
        }
    }

    fn on_report_event(&mut self, e: agent::Event) {
        let Some(job) = &mut self.report else { return };
        match e {
            agent::Event::Text(t) => job.text.push_str(&t),
            agent::Event::Tool(t) => job.status = t,
            agent::Event::TurnDone { ok } => {
                let job = self.report.take().unwrap();
                if !ok || job.text.trim().is_empty() {
                    self.notice = Some("The agent did not write a report.".into());
                    return;
                }
                let mut text = report::clean(&job.text);
                if let Some(c) = self.classes.as_ref().filter(|c| !c.hunks.is_empty()) {
                    text.push_str(&format!("\n## Coverage\n\n{}\n", c.coverage_note(self.ws)));
                }
                self.publish_report(&text);
            }
            agent::Event::Exited(err) => {
                self.report = None;
                self.agent = None;
                self.events = None;
                self.notice = Some(format!("The agent stopped while writing the report. {err}"));
            }
        }
    }

    fn publish_report(&mut self, text: &str) {
        let path = match report::save(text, self.cache.as_deref()) {
            Ok(p) => p,
            Err(e) => {
                self.notice = Some(format!("Could not save the report: {e}"));
                return;
            }
        };
        let _ = chat::copy_to_clipboard(&path.display().to_string());
        let root = self.config.as_ref().map(|c| c.root.clone()).unwrap_or_default();
        let range = format!("{}..{}", self.ws.manifest.base, self.ws.manifest.head);
        let sent = session::opener().map(|s| {
            let r = session::send(&s, &report::publish_message(&path, &root, &range));
            (s.name, r)
        });
        self.notice = Some(match sent {
            Some((name, Ok(()))) => format!(
                "Report saved to {} (path copied). Claude session '{name}' publishes it as an artifact; the link appears there.",
                path.display()
            ),
            Some((name, Err(e))) => format!(
                "Report saved to {} (path copied). Sending it to Claude session '{name}' failed: {e}",
                path.display()
            ),
            None => format!(
                "Report saved to {} (path copied). cicerone was not opened from a Claude session, so nothing publishes it: open it with /cicerone, or give the path to any agent.",
                path.display()
            ),
        });
    }

    /// The cache, or else a private directory made for this run.
    fn draft_path(&mut self) -> std::io::Result<PathBuf> {
        if let Some(d) = &self.cache {
            std::fs::create_dir_all(d)?;
            return Ok(d.join("github-review.md"));
        }
        if self.scratch.is_none() {
            let dir = tempfile::Builder::new()
                .prefix("cicerone-github-")
                .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
                .tempdir()?
                .keep();
            self.scratch = Some(dir);
        }
        Ok(self.scratch.as_ref().unwrap().join("github-review.md"))
    }

    /// Opens the GitHub review in the editor, then posts what the user saved.
    fn compose_review(&mut self, terminal: &mut DefaultTerminal) {
        if self.ws.manifest.head == "worktree" {
            self.notice = Some("Posting needs a commit range: the working tree has no PR commit.".into());
            return;
        }
        let commit = match crate::git::Repo::discover(&self.root)
            .and_then(|r| r.git(&["rev-parse", &self.ws.manifest.head]))
        {
            Ok(c) => c.trim().to_string(),
            Err(e) => {
                self.notice = Some(format!("Could not resolve the head commit: {e}"));
                return;
            }
        };
        let Some(pr) = self.pr.or_else(|| github::find_pr(&self.root, &commit)) else {
            self.notice = Some(format!(
                "No open PR has {} as its head. Open cicerone with --pr <n>.",
                self.ws.manifest.head
            ));
            return;
        };
        self.pr = Some(pr);
        let path = match self.draft_path() {
            Ok(p) => p,
            Err(e) => {
                self.notice = Some(format!("Could not make a place for the draft: {e}"));
                return;
            }
        };
        if !path.exists() {
            let intent = self.tours[AGENT].intent.clone();
            if let Err(e) = std::fs::write(&path, github::compose(self.ws, &self.findings, self.classes.as_ref(), &intent, pr)) {
                self.notice = Some(format!("Could not write the draft: {e}"));
                return;
            }
        }
        let edited = suspend(terminal, self.mouse, || {
            let editor = ["GIT_EDITOR", "VISUAL", "EDITOR"]
                .iter()
                .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
                .unwrap_or_else(|| "vi".into());
            std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("{editor} \"$1\""))
                .arg("sh")
                .arg(&path)
                .status()
        });
        if !edited.is_ok_and(|s| s.success()) {
            self.notice = Some("The editor failed. Nothing was posted; S opens the draft again.".into());
            return;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let draft = match github::parse(&text).and_then(|d| {
            if let Some(d) = &d {
                github::check(d, self.ws)?;
            }
            Ok(d)
        }) {
            Ok(Some(d)) => d,
            Ok(None) => {
                let _ = std::fs::remove_file(&path);
                self.notice = Some("Cancelled. Nothing was posted.".into());
                return;
            }
            Err(e) => {
                self.notice = Some(format!("Not posted: {e}. S opens your draft again."));
                return;
            }
        };
        let payload = github::payload(&draft, &commit);
        if std::env::var_os("CICERONE_POST_DRY_RUN").is_some() {
            let out = path.with_file_name("github-payload.json");
            let _ = std::fs::write(&out, serde_json::to_string_pretty(&payload).unwrap_or_default());
            self.notice = Some(format!("Dry run: the review for PR #{pr} is in {}. Nothing was posted.", out.display()));
            return;
        }
        match github::post(&self.root, pr, &payload) {
            Ok(url) => {
                let _ = std::fs::remove_file(&path);
                if let Some(d) = &self.cache {
                    let _ = std::fs::write(d.join("posted"), format!("{url}\n"));
                }
                self.notice = Some(format!("Posted the review on PR #{pr}: {url}"));
            }
            Err(e) => self.notice = Some(format!("Not posted: {e}. S opens your draft again.")),
        }
    }

    fn on_chat_event(&mut self, e: agent::Event) {
        match e {
            agent::Event::Text(t) => {
                self.turn_text = true;
                let (shown, items) = self.chat_filter.push(&t);
                self.chat_output(shown, items);
            }
            agent::Event::Tool(t) => self.chat.status = t,
            agent::Event::TurnDone { ok } => {
                let (shown, items) = self.chat_filter.finish();
                self.chat_output(shown, items);
                review::save(&self.findings, self.cache.as_deref());
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
        self.index_findings();
    }

    /// For each step, the findings whose lines its lenses and groups show.
    fn index_findings(&mut self) {
        let ws = self.ws;
        let ranges = |b: &Block| -> Vec<(String, usize, usize)> {
            let hunk = |id: &str| {
                ws.hunk(id)
                    .map(|h| (h.file.clone(), h.new.0, h.new.0 + h.new.1.saturating_sub(1)))
            };
            match b {
                Block::Lens(l) => {
                    if let Some(sym) = l.symbol.as_deref().and_then(|id| ws.symbol(id)) {
                        let deep = l.resolution >= Resolution::Skeleton
                            || (l.resolution == Resolution::Signature && sym.kind.is_type_level());
                        if sym.change == crate::manifest::Change::Removed || !deep {
                            return Vec::new();
                        }
                        return vec![(sym.file.clone(), sym.lines.0, sym.lines.1)];
                    }
                    if let Some(h) = l.hunk.as_deref().and_then(hunk) {
                        return vec![h];
                    }
                    match (&l.file, l.lines) {
                        (Some(f), Some((a, b))) => vec![(f.clone(), a, b)],
                        _ => Vec::new(),
                    }
                }
                Block::Group(g) => g.hunks.iter().filter_map(|h| hunk(h)).collect(),
                _ => Vec::new(),
            }
        };
        let tour = &self.tours[self.current];
        let mut placed = vec![false; self.findings.len()];
        self.step_findings = self
            .steps
            .iter()
            .map(|&(si, i)| {
                let step = &tour.stages[si].steps[i];
                let rs: Vec<(String, usize, usize)> = step.blocks.iter().flat_map(ranges).collect();
                let explicit: Vec<&str> = step
                    .blocks
                    .iter()
                    .filter_map(|b| match b {
                        Block::Finding(id) => Some(id.as_str()),
                        _ => None,
                    })
                    .collect();
                let is_review = tour.stages[si].title == review::TITLE;
                (0..self.findings.len())
                    .filter(|&k| {
                        let f = &self.findings[k];
                        if explicit.contains(&f.id.as_str()) {
                            return true;
                        }
                        if is_review || f.withdrawn || placed[k] || !rs.iter().any(|(file, a, b)| f.hits(file, *a, *b)) {
                            return false;
                        }
                        placed[k] = true;
                        true
                    })
                    .collect()
            })
            .collect();
    }

    fn jump_finding(&mut self, forward: bool) {
        let n = self.steps.len();
        let has = |p: usize| self.step_findings.get(p).is_some_and(|v| !v.is_empty());
        let target = if forward {
            (self.pos + 1..n).find(|&p| has(p))
        } else {
            (0..self.pos).rev().find(|&p| has(p))
        };
        if let Some(t) = target {
            self.go(t);
        }
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
        self.pump_helpers();
        let Some(rx) = &self.events else { return };
        let events: Vec<agent::Event> = rx.try_iter().collect();
        for e in events {
            self.on_event(e);
        }
    }

    fn on_event(&mut self, e: agent::Event) {
        if self.report.is_some() {
            self.on_report_event(e);
            return;
        }
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
                let phase = p.phase;
                if !ok {
                    p.error = Some("the agent reported an error".into());
                }
                self.apply(rest);
                self.next_phase(phase, ok);
                return;
            }
            agent::Event::Exited(err) => {
                let retry = p.phase == Phase::Review && self.resuming && self.findings.is_empty();
                p.error = Some(if err.is_empty() { "the agent stopped".into() } else { err });
                self.agent = None;
                self.events = None;
                if retry {
                    if let Some(c) = &mut self.config {
                        c.session = None;
                    }
                    self.planning = None;
                    self.start_review();
                    return;
                }
                if self.planning.as_ref().is_some_and(|p| matches!(p.phase, Phase::Plan | Phase::Cover)) {
                    self.finish_tour();
                }
                self.end_planning();
                return;
            }
        };
        self.apply(parsed);
    }

    fn next_phase(&mut self, done: Phase, ok: bool) {
        let review_only = self.planning.as_ref().is_some_and(|p| p.review_only);
        if done == Phase::Review {
            review::save(&self.findings, self.cache.as_deref());
        }
        if ok && !(review_only && done == Phase::Review) {
            let mut classes = self.classes.take().unwrap_or_default();
            let next = plan::after(done, self.ws, &self.tours[AGENT], &self.findings, &mut classes);
            if let plan::Next::Send(_, Phase::Plan) = &next {
                classes.save(self.cache.as_deref());
            }
            self.classes = Some(classes);
            if let plan::Next::Send(msg, phase) = next
                && self.turn(&msg, phase, phase.status().into())
            {
                return;
            }
        }
        if matches!(done, Phase::Plan | Phase::Cover) {
            self.finish_tour();
        }
        self.end_planning();
    }

    /// Sends one planning turn; false when the agent is gone.
    fn turn(&mut self, msg: &str, phase: Phase, status: String) -> bool {
        let Some(p) = &mut self.planning else { return false };
        let Some(a) = &mut self.agent else { return false };
        if a.send(msg).is_err() {
            return false;
        }
        p.phase = phase;
        p.status = status;
        p.parser = plan::Parser::default();
        true
    }

    fn end_planning(&mut self) {
        if self.planning.take().is_some() {
            review::save(&self.findings, self.cache.as_deref());
        }
        if let Some(q) = self.chat.pending.take() {
            self.dispatch(q);
        }
    }

    /// Starts a review turn for a tour that has none yet.
    pub fn review_now(mut self) -> Self {
        self.start_review();
        self
    }

    fn start_review(&mut self) {
        if self.config.is_none() || self.planning.is_some() {
            return;
        }
        if self.agent.is_none() && self.start_agent().is_err() {
            return;
        }
        let msg = self.with_primer(review::prompt());
        self.planning = Some(Planning {
            parser: plan::Parser::default(),
            phase: Phase::Review,
            status: "reviewing".into(),
            error: None,
            started: Instant::now(),
            review_only: true,
            helpers: None,
        });
        if !self.agent.as_mut().is_some_and(|a| a.send(&msg).is_ok()) {
            self.planning = None;
        }
    }

    pub fn with_github(mut self, root: PathBuf, pr: Option<u64>) -> Self {
        self.root = root;
        self.pr = pr;
        self
    }

    pub fn with_findings(mut self, findings: Vec<review::Finding>) -> Self {
        self.findings = findings;
        self.sync_findings();
        self
    }

    fn sync_findings(&mut self) {
        for t in &mut self.tours {
            if !t.stages.is_empty() {
                review::sync_stage(t, &self.findings);
            }
        }
        self.refresh_steps();
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
                plan::Parsed::Finding(f) => {
                    if let Some(f) = review::accept(f, self.ws, self.findings.len() + 1) {
                        self.findings.push(f);
                        self.sync_findings();
                    }
                }
                plan::Parsed::Update(_) | plan::Parsed::Withdraw(_) => {}
                plan::Parsed::Classes(cs) => {
                    let c = self.classes.get_or_insert_default();
                    for x in cs {
                        c.accept(x, self.ws);
                    }
                }
                plan::Parsed::Changes(changes) => self.classes.get_or_insert_default().changes = changes,
            }
        }
    }

    /// Completes and caches the agent tour; false when the agent gave none.
    fn finish_tour(&mut self) -> bool {
        if self.tours[AGENT].stages.is_empty() {
            let err = self.planning.as_ref().and_then(|p| p.error.clone());
            self.notice = Some(format!(
                "The agent did not return a tour{}. This is the automatic tour.",
                err.map(|e| format!(" ({e})")).unwrap_or_default()
            ));
            return false;
        }
        plan::complete(&mut self.tours[AGENT], self.ws, self.classes.as_ref());
        self.agent_ready = true;
        plan::save(&self.tours[AGENT], self.cache.as_deref());
        self.sync_findings();
        true
    }

    pub fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            self.pump();
            terminal.draw(|f| self.draw(f))?;
            let tick = if self.planning.is_some() || self.report.is_some() { 80 } else { 250 };
            if event::poll(Duration::from_millis(tick))? {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => {
                        self.key(k);
                        if std::mem::take(&mut self.compose) {
                            self.compose_review(terminal);
                        }
                    }
                    Event::Mouse(m) => self.mouse(m),
                    _ => {}
                }
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
                    "pgup" => KeyCode::PageUp,
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

    fn mouse(&mut self, m: MouseEvent) {
        let up = match m.kind {
            MouseEventKind::ScrollUp => true,
            MouseEventKind::ScrollDown => false,
            _ => return,
        };
        let at = ratatui::layout::Position::new(m.column, m.row);
        if self.chat.open && self.chat_area.contains(at) {
            self.chat_scroll_by(if up { 3 } else { -3 });
        } else if self.main_area.contains(at) {
            self.scroll_by(if up { -3 } else { 3 });
        }
    }

    fn chat_scroll_by(&mut self, d: isize) {
        self.chat.scroll = (self.chat.scroll as isize + d).clamp(0, self.chat_max as isize) as usize;
    }

    fn copy_last_answer(&mut self) {
        let last = self.chat.msgs.iter().rev().find(|m| m.role == Role::Agent);
        self.chat.flash = Some(match last {
            Some(m) => match chat::copy_to_clipboard(m.text.trim()) {
                Ok(()) => format!("copied {} characters", m.text.trim().chars().count()),
                Err(e) => format!("copy failed: {e}"),
            },
            None => "nothing to copy yet".into(),
        });
    }

    fn toggle_mouse(&mut self) {
        use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
        self.mouse = !self.mouse;
        let _ = if self.mouse {
            ratatui::crossterm::execute!(std::io::stdout(), EnableMouseCapture)
        } else {
            ratatui::crossterm::execute!(std::io::stdout(), DisableMouseCapture)
        };
    }

    fn chat_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        self.chat.flash = None;
        match k.code {
            KeyCode::Char('y') if ctrl => self.copy_last_answer(),
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
            KeyCode::Up => self.chat_scroll_by(1),
            KeyCode::Down => self.chat_scroll_by(-1),
            KeyCode::PageUp => self.chat_scroll_by(self.chat_page as isize),
            KeyCode::PageDown => self.chat_scroll_by(-(self.chat_page as isize)),
            KeyCode::Char('b') if ctrl => self.chat_scroll_by(self.chat_page as isize),
            KeyCode::Char('f') if ctrl => self.chat_scroll_by(-(self.chat_page as isize)),
            KeyCode::Char(c) if !ctrl => self.chat.input.push(c),
            _ => {}
        }
    }

    fn open_chat(&mut self, about_lens: bool) {
        self.chat.open = true;
        self.chat.about = None;
        let finding = self.step_blocks().iter().find_map(|b| match b {
            Block::Finding(id) => self.findings.iter().find(|f| &f.id == id).cloned(),
            _ => None,
        });
        if about_lens && let Some(f) = finding {
            self.chat.about = Some((
                format!("{} · {}", f.id, f.title),
                format!("review finding {} ({} at {}:{}-{})", f.id, f.title, f.file, f.lines.0, f.lines.1),
            ));
            return;
        }
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
            KeyCode::Char(']') => self.jump_finding(true),
            KeyCode::Char('[') => self.jump_finding(false),
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
            KeyCode::Char('m') => self.toggle_mouse(),
            KeyCode::Char('E') => self.start_report(),
            KeyCode::Char('S') => self.compose = true,
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
            let count: usize = positions
                .iter()
                .flat_map(|&p| self.step_findings.get(p).into_iter().flatten())
                .filter(|&&k| !self.findings[k].withdrawn)
                .count();
            let badge = if count > 0 { format!(" ◆{count}") } else { String::new() };
            let max = (area.width as usize).saturating_sub(5 + badge.chars().count());
            let title = if st.title.chars().count() > max {
                format!("{}…", st.title.chars().take(max.saturating_sub(1)).collect::<String>())
            } else {
                st.title.clone()
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {mark} "), style),
                Span::styled(title, if si == cur.0 { style } else { text() }),
                Span::styled(badge, Style::default().fg(PEACH)),
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
        if let Some(job) = &self.report {
            const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let i = (job.started.elapsed().as_millis() / 80) as usize % SPIN.len();
            out.push(Line::from(vec![
                Span::styled(format!(" {} ", SPIN[i]), Style::default().fg(MAUVE)),
                Span::styled(
                    format!("report · {}s", job.started.elapsed().as_secs()),
                    Style::default().fg(MAUVE),
                ),
            ]));
            let status: String = job.status.chars().take(w.saturating_sub(6)).collect();
            out.push(Line::from(Span::styled(format!("   {status}"), dim())));
        }
        if let Some(p) = &self.planning {
            const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let i = (p.started.elapsed().as_millis() / 80) as usize % SPIN.len();
            let status: String = p.status.chars().take(w.saturating_sub(6)).collect();
            out.push(Line::from(vec![
                Span::styled(format!(" {} ", SPIN[i]), Style::default().fg(MAUVE)),
                Span::styled(
                    format!("{} · {}s", p.phase.status(), p.started.elapsed().as_secs()),
                    Style::default().fg(MAUVE),
                ),
            ]));
            out.push(Line::from(Span::styled(format!("   {status}"), dim())));
            if !p.review_only {
                let at = match p.phase {
                    Phase::Split | Phase::Review => 0,
                    Phase::Classify { .. } => 1,
                    Phase::Plan | Phase::Cover => 2,
                };
                let open = self.findings.iter().filter(|f| !f.withdrawn).count();
                let classified = self.classes.as_ref().map_or(0, |c| c.hunks.len());
                let total = self.ws.manifest.hunks.len();
                let review = match &p.helpers {
                    Some(h) => format!("review · parts {}/{} · {} raw findings", h.done, h.parts.len(), h.findings.len()),
                    None => format!("review · {open} findings"),
                };
                for (k, name) in [
                    review,
                    format!("classify · {classified}/{total} hunks"),
                    "tour".to_string(),
                ]
                .into_iter()
                .enumerate()
                {
                    let (mark, st) = match k.cmp(&at) {
                        std::cmp::Ordering::Less => ("✓", Style::default().fg(GREEN)),
                        std::cmp::Ordering::Equal => ("›", Style::default().fg(MAUVE)),
                        std::cmp::Ordering::Greater => ("·", dim()),
                    };
                    out.push(Line::from(vec![
                        Span::styled(format!("   {mark} "), st),
                        Span::styled(name, if k > at { dim() } else { Style::default().fg(SUBTEXT) }),
                    ]));
                }
            }
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
        let here: Vec<review::Finding> = self
            .step_findings
            .get(self.pos)
            .map(|v| v.iter().map(|&k| self.findings[k].clone()).collect())
            .unwrap_or_default();
        let in_review = stage.title == review::TITLE;
        if !here.is_empty() && !in_review {
            out.push(Line::from(Span::styled(
                format!("◆ {} review finding{} in this step", here.len(), if here.len() == 1 { "" } else { "s" }),
                Style::default().fg(PEACH).add_modifier(Modifier::BOLD),
            )));
            for f in &here {
                let label = format!("  {} ", f.severity.label());
                let pad = " ".repeat(label.chars().count());
                out.extend(view::wrapped(
                    vec![Span::styled(label, view::severity_style(f.severity))],
                    vec![Span::raw(pad)],
                    &f.title,
                    text(),
                    w,
                ));
            }
            out.push(Line::default());
        }
        let cards: Vec<&str> = if in_review { Vec::new() } else { here.iter().map(|f| f.id.as_str()).collect() };
        let all: Vec<review::Finding> = self.findings.clone();
        let marks: Vec<&review::Finding> = all.iter().filter(|f| !f.withdrawn).collect();
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
                Block::Finding(id) => {
                    if let Some(f) = self.findings.iter().find(|f| &f.id == id) {
                        out.extend(view::finding_card(f, Vec::new(), w));
                        out.extend(view::finding_history(f, w));
                    }
                }
                Block::Group(g) => {
                    let room = w.saturating_sub(view::GROUP_GUTTER);
                    hmax = hmax.max(view::group_widest(self.ws, g).saturating_sub(room));
                    out.extend(view::group(self.ws, g, w, hs, &marks, &cards));
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
                    out.extend(view::lens(&v, li == self.focus, w, hs, &marks, &cards));
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
        self.main_area = area;
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

    fn draw_chat(&mut self, f: &mut Canvas, area: Rect) {
        self.chat_area = area;
        let busy = match (&self.chat.flash, self.chat.busy) {
            (Some(f), _) => format!(" · {f}"),
            (None, true) => " · answering".into(),
            (None, false) => String::new(),
        };
        let block = Frame::default()
            .borders(Borders::TOP)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(MAUVE))
            .title(Span::styled(format!(" ✦ chat{busy} "), Style::default().fg(MAUVE).add_modifier(Modifier::BOLD)))
            .padding(Padding::horizontal(2));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let about_h = u16::from(self.chat.about.is_some());
        let typed = self.chat.input_lines(inner.width as usize);
        let input_h = typed.len().clamp(1, MAX_INPUT_LINES);
        let [log, about, input] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(about_h),
            Constraint::Length(input_h as u16),
        ])
        .areas(inner);
        let lines = self.chat.transcript(log.width as usize);
        let h = log.height as usize;
        self.chat_page = (h / 2).max(1);
        self.chat_max = lines.len().saturating_sub(h);
        self.chat.scroll = self.chat.scroll.min(self.chat_max);
        let end = lines.len().saturating_sub(self.chat.scroll);
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
        let skip = typed.len().saturating_sub(input_h);
        f.render_widget(Paragraph::new(typed[skip..].to_vec()), input);
    }

    fn draw_status(&self, f: &mut Canvas, area: Rect) {
        if self.chat.open {
            let keys = [
                ("enter", "send"),
                ("esc", "close"),
                ("↑/↓ pgup/pgdn wheel", "scroll"),
                ("ctrl-y", "copy answer"),
                ("ctrl-u", "clear"),
            ];
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
            ("]/[", "finding"),
            ("tab", "lens"),
            ("+/-", "zoom"),
            ("d", "diff"),
            ("c", "chat"),
            ("a", "ask"),
            ("A", "tour"),
            ("E", "report"),
            ("S", "post"),
            ("m", if self.mouse { "mouse" } else { "mouse off" }),
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
        ("]  [", "next / previous step with review findings"),
        ("j  k  ↓  ↑", "scroll"),
        ("ctrl-d  ctrl-u", "half page"),
        ("g  G", "top / bottom"),
        ("tab  shift-tab", "focus next / previous lens"),
        ("+  -", "zoom the lens: name, signature, skeleton, full"),
        ("d", "show or hide removed lines and markers"),
        ("A", "switch between the agent tour and the automatic tour"),
        ("c", "open the chat with the agent"),
        ("ctrl-y", "in the chat: copy the last answer to the clipboard"),
        ("m", "mouse capture off (select text) / on (wheel scroll)"),
        ("E", "the agent writes a review report; your Claude session publishes it"),
        ("S", "post the review on the PR: edit it first, as Claude or as you"),
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

/// Runs `f` with the terminal handed back to the shell, for an editor.
fn suspend<T>(terminal: &mut DefaultTerminal, mouse: bool, f: impl FnOnce() -> T) -> T {
    use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    let out = f();
    *terminal = ratatui::init();
    if mouse {
        let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    }
    let _ = terminal.clear();
    out
}
