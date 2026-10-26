use crate::classes::Classes;
use crate::lens::LensSpec;
use crate::manifest::Workspace;
use crate::tour::{self, Block, Gloss, Stage, Tour};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fmt::Write;
use std::path::PathBuf;

pub const SYSTEM: &str = r#"You are the reviewer and the guide inside cicerone, a terminal tool that walks a human reviewer through a code change in stages.

The session has these turns, in order. Each message says which turn it is.
1. REVIEW: you review the whole change and report findings.
2. CLASSIFY: you give every hunk a class.
3. PLAN: you plan the tour from your review and your classes.
4. COVERAGE: cicerone can ask you to place hunks that the tour left out.
5. CHAT: the reviewer asks questions while reading the tour.

The reviewer must understand early what the change does and why, then go deeper step by step, and skim the mechanical parts. Code is shown as it is, at a chosen resolution, with your short explanations beside it.

PLANNING OUTPUT: JSON Lines only. One JSON object per line. No prose, no code fences, no blank lines.
First line: {"tour":{"title":"...","intent":"one sentence: what the change does and why","glossary":[{"name":"...","meaning":"one short sentence","symbol":"S3"}]}}
Then one line per stage, written as soon as the stage is ready: {"stage":{"title":"...","steps":[{"title":"...","text":"markdown","blocks":[...]}]}}

Blocks: "map" (the change map) | "glossary" (the glossary from the first line) | {"text":"markdown"} | {"lens":{...}} | {"pattern":"P1"} | {"group":{"title":"...","text":"markdown","hunks":["H3","H9"]}}
A group lists many hunks compactly: only their changed lines, no context. Use it for hunks that make the same kind of change.
Lens target, exactly one of: {"symbol":"S4"} | {"hunk":"H12"} | {"file":"path","lines":[a,b]}
Lens options: "resolution": "name" | "signature" | "skeleton" | "full" (default full). "holes": [{"lines":[a,b],"label":"..."}] names the parts a skeleton or signature folds. "focus": [[a,b]] highlights lines. "notes": [{"line":n,"text":"..."}] puts a margin note under a line. "diff": false hides removed lines.
Resolutions: name = one line. signature = the type signature, body folded (for a changed type: the changed lines with context). skeleton = control flow only: equations, guards, case branches, if branches, let/where bindings, do statements; the rest folds into holes. full = every line, removed lines in place.
All line numbers are head line numbers.

RULES
1. Wide to narrow. Stage 1: what the change does and why, as a short text and the "map" block. Stage 2: vocabulary, the "glossary" block. Then shapes: new and changed types and signatures. Then the core logic: skeletons first, then the full code of the parts that matter. Then callers and wiring. Tests last.
2. Names before uses: a step must not depend on a symbol that a later step introduces. The manifest "order" and "uses" fields give the dependencies.
3. A symbol can come back in a later stage at a higher resolution. That is good: the reader sees a known shape open up.
4. Text is short. Say why, and how the parts fit together. Do not narrate the code line by line. Markdown: `code`, **bold**, "- " bullets.
5. Use only the symbol, hunk, and pattern ids in the manifest.
6. Leave out the hunks that you classified "mechanical": cicerone appends a mechanical stage from your classes. Every other hunk must appear in the tour, in a lens or in a group.
   When many hunks make the same kind of change (for example, every handler moves to a new API), show one or two of them as lenses, then put all of them in one group in the same step, with a text that names the shared change and any site that differs.
7. Zoom, do not dump. This is the point of the tool:
   - Prefer symbol lenses. Use a hunk lens only for a hunk outside every symbol (imports, exports, non-code files).
   - In the shapes stage, show functions at "signature" and changed types at "signature". Show a new type at "full" only when it is short.
   - Show a function longer than 15 lines first at "skeleton", with a label on each hole that hides something the reader must know. Show it at "full" only in a later step, with "focus" on the lines that matter.
   - Use "notes" for the one to three lines in a step that a reviewer must not miss.
   - Symbol lenses exist only for files with symbols in the manifest. For any other file, zoom with a file lens: "resolution": "skeleton" and "holes" that fold the parts the reader can skip at first, each with a label. A hole folds its lines at "skeleton" and shows them at "full". Keep the holes inside the lens lines.
8. To read more context, use Read, Grep, and Glob on the FILES directory named in the material, and on the repository working tree. The working tree can differ from head.
9. 4 to 8 stages. 1 to 4 lenses per step.
10. Show the code of each finding at "skeleton" or "full" in the step where the reader first meets that code, so the reader sees the problem in context. cicerone puts the finding next to its lines.
11. Show a "docs" hunk in a lens of its own at "full", in a step with the code that it describes or in a docs step, never only in a group. A reader must be able to read a new or rewritten page as a page.

COVERAGE
After the plan, cicerone can send the hunks that the tour does not show yet. Answer with JSON Lines only: one or more {"stage":{...}} lines whose steps sort those hunks into "group" blocks by what they do. Each listed hunk goes into exactly one group. Use lenses only for a hunk that a reviewer must read in full.

CHAT
After the plan, the reviewer can send questions. Each question names the step and the lens they are looking at, and lists the open review findings with their ids (F1, F2, ...). Answer in short markdown for a terminal: plain sentences, `code`, bullets. No headings, no tables.
When the conversation shows that a finding is wrong, too severe, too mild, or needs a better text or anchor, change it: put one JSON object on its own line, anywhere in the answer:
{"finding_update":{"id":"F3","severity":"change","title":"...","body":"...","lines":[a,b],"reason":"why it changed"}} (give only the fields that change, and always a reason)
{"finding_withdraw":{"id":"F3","reason":"why it is not a problem"}}
{"finding":{...}} for a new problem, in the review format.
Do this only when the evidence supports it, and say so in the text too. Never put other JSON in an answer.
An intended behavior change is still a finding: when the reviewer confirms that it is intended, change a bug or risk about it to "change", never to "nit"."#;

const MAX_DIFF: usize = crate::split::WHOLE;

pub fn prompt(ws: &Workspace) -> String {
    format!(
        "REVIEW of the change {}..{}.\n\n{}\n\n{}",
        ws.manifest.base,
        ws.manifest.head,
        material(ws),
        crate::review::prompt()
    )
}

pub fn classify_prompt() -> String {
    format!("CLASSIFY\n\n{}", crate::classes::PROMPT)
}

pub fn plan_prompt(findings: &[crate::review::Finding]) -> String {
    let mut s = String::from(
        "PLAN\n\nNow plan the tour, as PLANNING OUTPUT and RULES in the system prompt say. Use your review and your classes. Show every hunk that you did not classify \"mechanical\".\n",
    );
    if !findings.is_empty() {
        s.push_str("\nYour findings, to show in context (rule 10):\n");
        for f in findings {
            let _ = writeln!(s, "{} {} {} ({}:{}-{})", f.id, f.severity.label(), f.title, f.file, f.lines.0, f.lines.1);
        }
    }
    s
}

/// The context for a chat session that did not plan the tour itself.
pub fn primer(ws: &Workspace, tour: &Tour) -> String {
    format!(
        "Do not plan a tour. The reviewer reads this tour of the change {}..{} and asks you questions about it.\n\nTOUR\n{}\n\n{}",
        ws.manifest.base,
        ws.manifest.head,
        serde_json::to_string(tour).unwrap_or_default(),
        material(ws)
    )
}

fn material(ws: &Workspace) -> String {
    let m = &ws.manifest;
    let mut s = String::new();
    if let Some(d) = &ws.snapshot {
        let _ = writeln!(
            s,
            "FILES\nThe full base and head versions of the changed files are at {0}/base/<path> and {0}/head/<path>.\n",
            d.display()
        );
    }
    let _ = writeln!(s, "MANIFEST\n{}\n", serde_json::to_string(m).unwrap_or_default());
    let _ = writeln!(
        s,
        "DIFF (every hunk. \"likely mechanical\" is a hint from a syntax detector, not a fact: check it)"
    );
    let mut left_out = 0;
    for f in &ws.files {
        let mut header = false;
        for h in &f.diff.hunks {
            let mech = ws.hunk(&h.id).and_then(|x| x.mechanical.as_ref());
            let generated = matches!(mech, Some(crate::mechanical::Mechanical::Generated));
            let text: String = if generated {
                "(generated file: the lines are left out. Read the file under FILES if you need them.)\n".into()
            } else {
                h.lines.iter().map(|l| format!("{l}\n")).collect()
            };
            if s.len() + text.len() > MAX_DIFF {
                left_out += 1;
                continue;
            }
            if !header {
                let _ = writeln!(s, "\n### {}", f.diff.path);
                header = true;
            }
            let hint = mech
                .map(|m| format!("  likely mechanical: {}", crate::classes::hint(ws, m)))
                .unwrap_or_default();
            let _ = writeln!(
                s,
                "@@ {} -{},{} +{},{}{hint}",
                h.id, h.old_start, h.old_len, h.new_start, h.new_len
            );
            s.push_str(&text);
        }
    }
    if left_out > 0 {
        let _ = writeln!(s, "\n({left_out} hunks did not fit. Read the files under FILES.)");
    }
    s
}

#[derive(Deserialize)]
struct Header {
    title: String,
    #[serde(default)]
    intent: String,
    #[serde(default)]
    glossary: Vec<Gloss>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Item {
    Tour(Header),
    Stage(Stage),
    Finding(crate::review::Finding),
    FindingUpdate(crate::review::Update),
    FindingWithdraw(crate::review::Withdraw),
    Class(crate::classes::Line),
    Changes(Vec<String>),
}

#[derive(Default)]
pub struct Parser {
    buf: String,
}

pub enum Parsed {
    Header { title: String, intent: String, glossary: Vec<Gloss> },
    Stage(Stage),
    Finding(crate::review::Finding),
    Update(crate::review::Update),
    Withdraw(crate::review::Withdraw),
    Classes(Vec<crate::classes::Class>),
    Changes(Vec<String>),
}

impl Parser {
    pub fn push(&mut self, text: &str) -> Vec<Parsed> {
        self.buf.push_str(text);
        let mut out = Vec::new();
        while let Some(i) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=i).collect();
            out.extend(parse_line(&line));
        }
        out
    }

    pub fn finish(&mut self) -> Vec<Parsed> {
        let rest = std::mem::take(&mut self.buf);
        parse_line(&rest).into_iter().collect()
    }
}

pub fn parse_line(line: &str) -> Option<Parsed> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    match serde_json::from_str::<Item>(line).ok()? {
        Item::Tour(h) => Some(Parsed::Header {
            title: h.title,
            intent: h.intent,
            glossary: h.glossary,
        }),
        Item::Stage(s) => Some(Parsed::Stage(s)),
        Item::Finding(f) => Some(Parsed::Finding(f)),
        Item::FindingUpdate(u) => Some(Parsed::Update(u)),
        Item::FindingWithdraw(w) => Some(Parsed::Withdraw(w)),
        Item::Class(c) => Some(Parsed::Classes(c.classes())),
        Item::Changes(c) => Some(Parsed::Changes(c)),
    }
}

pub const LEFT_OUT: &str = "Left out by the tour";

fn mechanical(ws: &Workspace, classes: Option<&Classes>, id: &str) -> bool {
    match classes {
        Some(c) => c.is_mechanical(id),
        None => ws.hunk(id).is_some_and(|h| h.mechanical.is_some()),
    }
}

/// Hunks that are not mechanical and that no lens or group of the tour
/// shows. The agent's classes decide what is mechanical when there are any.
pub fn uncovered<'a>(tour: &Tour, ws: &'a Workspace, classes: Option<&Classes>) -> Vec<&'a crate::manifest::HunkEntry> {
    let shown = shown_hunks(tour, ws);
    ws.manifest
        .hunks
        .iter()
        .filter(|h| !mechanical(ws, classes, &h.id) && !shown.contains(h.id.as_str()))
        .collect()
}

pub fn coverage_prompt(ws: &Workspace, missing: &[&crate::manifest::HunkEntry]) -> String {
    let mut s = format!(
        "The tour does not show these {} hunks yet. Sort them into groups by what they do, as COVERAGE in the system prompt says. Their diff is in the first message.\n",
        missing.len()
    );
    for h in missing {
        let names: Vec<&str> = h
            .symbols
            .iter()
            .filter_map(|id| ws.symbol(id))
            .map(|x| x.name.as_str())
            .collect();
        let _ = writeln!(s, "{} {}:{} +{} -{} {}", h.id, h.file, h.new.0, h.added, h.removed, names.join(","));
    }
    s
}

/// Rebuilds the stages that cicerone owns, so a cached tour follows the
/// current mechanical analysis.
pub fn refresh(tour: &mut Tour, ws: &Workspace, classes: Option<&Classes>) {
    tour.stages.retain(|s| {
        s.title != "Mechanical" && s.title != LEFT_OUT && s.title != "Not covered yet" && s.title != crate::review::TITLE
    });
    complete(tour, ws, classes);
}

/// Appends the mechanical stage and a stage for every hunk that the tour
/// still does not show, so nothing escapes the review.
pub fn complete(tour: &mut Tour, ws: &Workspace, classes: Option<&Classes>) {
    if let Some(c) = classes {
        promote_docs(tour, c);
    }
    if !tour.stages.iter().any(|s| s.title == "Mechanical") {
        let mech = match classes {
            Some(c) => mechanical_stage(ws, c),
            None => tour::auto(ws).stages.into_iter().find(|s| s.title == "Mechanical"),
        };
        tour.stages.extend(mech);
    }
    let docs = |id: &str| classes.is_some_and(|c| c.kind(id) == Some(crate::classes::Kind::Docs));
    let mut missing: Vec<(&str, Vec<String>)> = Vec::new();
    for h in uncovered(tour, ws, classes) {
        match missing.iter_mut().find(|(f, _)| *f == h.file) {
            Some((_, v)) => v.push(h.id.clone()),
            None => missing.push((&h.file, vec![h.id.clone()])),
        }
    }
    if !missing.is_empty() {
        tour.stages.push(Stage {
            title: LEFT_OUT.into(),
            steps: missing
                .into_iter()
                .map(|(file, hunks)| {
                    let (pages, rest): (Vec<String>, Vec<String>) = hunks.into_iter().partition(|h| docs(h));
                    let mut blocks: Vec<Block> = pages.iter().map(|h| Block::Lens(LensSpec::hunk(h))).collect();
                    if !rest.is_empty() {
                        blocks.push(Block::Group(tour::Group {
                            title: String::new(),
                            text: String::new(),
                            hunks: rest,
                        }));
                    }
                    tour::Step {
                        title: tour::short(file),
                        text: String::new(),
                        blocks,
                    }
                })
                .collect(),
        });
    }
}

/// Takes docs hunks out of groups and shows each in a lens of its own in
/// the same step, so a page reads as a page and not as changed lines.
fn promote_docs(tour: &mut Tour, c: &Classes) {
    let docs = |h: &str| c.kind(h) == Some(crate::classes::Kind::Docs);
    let lensed: BTreeSet<String> = tour
        .stages
        .iter()
        .flat_map(|s| &s.steps)
        .flat_map(|s| &s.blocks)
        .filter_map(|b| match b {
            Block::Lens(l) => l.hunk.clone(),
            _ => None,
        })
        .collect();
    for step in tour.stages.iter_mut().flat_map(|s| &mut s.steps) {
        let mut blocks = Vec::with_capacity(step.blocks.len());
        for b in std::mem::take(&mut step.blocks) {
            let Block::Group(mut g) = b else {
                blocks.push(b);
                continue;
            };
            let (pages, rest): (Vec<String>, Vec<String>) = g.hunks.drain(..).partition(|h| docs(h));
            blocks.extend(
                pages
                    .iter()
                    .filter(|h| !lensed.contains(*h))
                    .map(|h| Block::Lens(LensSpec::hunk(h))),
            );
            if !rest.is_empty() {
                g.hunks = rest;
                blocks.push(Block::Group(g));
            }
        }
        step.blocks = blocks;
    }
}

fn mechanical_stage(ws: &Workspace, c: &Classes) -> Option<Stage> {
    let mut steps = Vec::new();
    let mut shown = BTreeSet::new();
    for p in &ws.manifest.patterns {
        let mine: Vec<&String> = p.hunks.iter().filter(|h| c.is_mechanical(h)).collect();
        let Some(first) = mine.first() else { continue };
        shown.extend(mine.iter().map(|h| h.as_str()));
        let mut blocks = vec![Block::Pattern(p.id.clone()), Block::Lens(LensSpec::hunk(first))];
        blocks.extend(p.outliers.iter().map(|o| Block::Lens(LensSpec::hunk(&o.hunk))));
        steps.push(tour::Step {
            title: format!("Pattern {}", p.id),
            text: String::new(),
            blocks,
        });
    }
    for (reason, hunks) in c.mechanical_groups() {
        let hunks: Vec<String> = hunks.into_iter().filter(|h| !shown.contains(h.as_str())).collect();
        if hunks.is_empty() {
            continue;
        }
        let title = if reason.is_empty() { "Mechanical".to_string() } else { reason.clone() };
        steps.push(tour::Step {
            title,
            text: String::new(),
            blocks: vec![Block::Group(tour::Group {
                title: String::new(),
                text: String::new(),
                hunks,
            })],
        });
    }
    (!steps.is_empty()).then(|| Stage {
        title: "Mechanical".into(),
        steps,
    })
}

fn shown_hunks<'a>(tour: &Tour, ws: &'a Workspace) -> BTreeSet<&'a str> {
    let mut shown = BTreeSet::new();
    for b in tour.stages.iter().flat_map(|s| &s.steps).flat_map(|s| &s.blocks) {
        if let Block::Group(g) = b {
            for id in &g.hunks {
                if let Some(h) = ws.hunk(id) {
                    shown.insert(h.id.as_str());
                }
            }
            continue;
        }
        let Block::Lens(l) = b else { continue };
        for h in &ws.manifest.hunks {
            let hit = l.hunk.as_deref() == Some(h.id.as_str())
                || l.symbol.as_deref().is_some_and(|s| h.symbols.iter().any(|x| x == s))
                || (l.file.as_deref() == Some(h.file.as_str())
                    && l.lines.is_some_and(|(a, b)| h.new.0 <= b && a < h.new.0 + h.new.1.max(1)));
            if hit {
                shown.insert(h.id.as_str());
            }
        }
    }
    shown
}

/// Writes the base and head version of each changed file under `dir`, so
/// the agent can read them without a shell.
pub fn snapshot(ws: &Workspace, dir: &std::path::Path) -> std::io::Result<()> {
    use std::path::Component;
    for f in &ws.files {
        for (side, path, content) in [
            ("base", f.diff.base_path(), &f.base),
            ("head", f.diff.path.as_str(), &f.head),
        ] {
            let rel = std::path::Path::new(path);
            let Some(content) = content else { continue };
            if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
                continue;
            }
            let target = dir.join(side).join(rel);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, content)?;
        }
    }
    Ok(())
}

pub fn save(tour: &Tour, cache: Option<&std::path::Path>) {
    if let Some(dir) = cache
        && std::fs::create_dir_all(dir).is_ok()
        && let Ok(json) = serde_json::to_string_pretty(tour)
    {
        let _ = std::fs::write(dir.join("tour.json"), json);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Split,
    Review,
    Classify { retried: bool },
    Plan,
    Cover,
}

impl Phase {
    pub fn status(self) -> &'static str {
        match self {
            Phase::Split => "reviewing in parts",
            Phase::Review => "reviewing",
            Phase::Classify { .. } => "classifying the hunks",
            Phase::Plan => "planning the tour",
            Phase::Cover => "placing the rest",
        }
    }
}

pub enum Next {
    Send(String, Phase),
    Done,
}

/// The turn after `done`. A classify turn that leaves hunks out is asked
/// once more; after that, the detector fills the gaps.
pub fn after(
    done: Phase,
    ws: &Workspace,
    tour: &Tour,
    findings: &[crate::review::Finding],
    classes: &mut Classes,
) -> Next {
    match done {
        Phase::Split => Next::Done,
        Phase::Review => Next::Send(classify_prompt(), Phase::Classify { retried: false }),
        Phase::Classify { retried } => {
            let missing = classes.missing(ws);
            if !retried && !missing.is_empty() {
                return Next::Send(crate::classes::retry_prompt(&missing), Phase::Classify { retried: true });
            }
            classes.fill_missing(ws);
            Next::Send(plan_prompt(findings), Phase::Plan)
        }
        Phase::Plan => {
            let missing = uncovered(tour, ws, Some(classes));
            if tour.stages.is_empty() || missing.is_empty() {
                return Next::Done;
            }
            Next::Send(coverage_prompt(ws, &missing), Phase::Cover)
        }
        Phase::Cover => Next::Done,
    }
}

pub fn run_headless(
    ws: &Workspace,
    agent: &mut crate::agent::Agent,
    rx: std::sync::mpsc::Receiver<crate::agent::Event>,
    cache: Option<&std::path::Path>,
) -> anyhow::Result<Tour> {
    use crate::agent::Event;
    use crate::review;
    let mut parser = Parser::default();
    let mut tour = Tour {
        title: String::new(),
        intent: String::new(),
        range: None,
        glossary: Vec::new(),
        stages: Vec::new(),
    };
    let mut findings: Vec<review::Finding> = Vec::new();
    let mut classes = Classes::default();
    let mut phase = Phase::Review;
    eprintln!("{}", phase.status());
    for e in rx {
        let items = match e {
            Event::Text(t) => parser.push(&t),
            Event::Tool(t) => {
                eprintln!("tool: {}", crate::clean(&t));
                continue;
            }
            Event::TurnDone { ok } => {
                let rest = parser.finish();
                apply_headless(rest, ws, &mut tour, &mut findings, &mut classes);
                parser = Parser::default();
                anyhow::ensure!(ok, "the agent reported an error");
                if phase == Phase::Review {
                    review::save(&findings, cache);
                }
                match after(phase, ws, &tour, &findings, &mut classes) {
                    Next::Send(msg, next) => {
                        if matches!(next, Phase::Plan) {
                            classes.save(cache);
                        }
                        eprintln!("{}", next.status());
                        agent.send(&msg)?;
                        phase = next;
                    }
                    Next::Done => break,
                }
                continue;
            }
            Event::Exited(err) => anyhow::bail!("the agent stopped: {err}"),
        };
        apply_headless(items, ws, &mut tour, &mut findings, &mut classes);
    }
    anyhow::ensure!(!tour.stages.is_empty(), "the agent returned no stages");
    complete(&mut tour, ws, Some(&classes));
    save(&tour, cache);
    review::sync_stage(&mut tour, &findings);
    Ok(tour)
}

fn apply_headless(
    items: Vec<Parsed>,
    ws: &Workspace,
    tour: &mut Tour,
    findings: &mut Vec<crate::review::Finding>,
    classes: &mut Classes,
) {
    for it in items {
        match it {
            Parsed::Header { title, intent, glossary } => {
                tour.title = title;
                tour.intent = intent;
                tour.glossary = glossary;
            }
            Parsed::Stage(s) => {
                eprintln!("stage: {}", crate::clean(&s.title));
                tour.stages.push(s);
            }
            Parsed::Update(_) | Parsed::Withdraw(_) => {}
            Parsed::Finding(f) => {
                if let Some(f) = crate::review::accept(f, ws, findings.len() + 1) {
                    eprintln!("finding: {} {}", f.severity.label(), crate::clean(&f.title));
                    findings.push(f);
                }
            }
            Parsed::Classes(cs) => {
                for c in cs {
                    classes.accept(c, ws);
                }
            }
            Parsed::Changes(c) => classes.changes = c,
        }
    }
}

pub fn cache_dir(repo_root: &std::path::Path, base: &str, head: &str) -> Option<PathBuf> {
    let root = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let name = repo_root.file_name()?.to_string_lossy().to_string();
    Some(root.join("cicerone").join(format!("{name}-{base}-{head}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_reads_split_lines_and_skips_prose() {
        let mut p = Parser::default();
        let mut got = p.push("Sure, here it is:\n{\"tour\":{\"title\":\"T\",\"int");
        got.extend(p.push("ent\":\"I\"}}\n{\"stage\":{\"title\":\"A\",\"steps\":[{\"title\":\"s\",\"blocks\":[\"map\",{\"lens\":{\"symbol\":\"S1\",\"resolution\":\"skeleton\"}}]}]}}"));
        got.extend(p.finish());
        assert_eq!(got.len(), 2);
        assert!(matches!(&got[0], Parsed::Header { title, intent, .. } if title == "T" && intent == "I"));
        let Parsed::Stage(s) = &got[1] else { panic!() };
        assert_eq!(s.steps[0].blocks.len(), 2);
        assert!(matches!(&s.steps[0].blocks[1], Block::Lens(l) if l.symbol.as_deref() == Some("S1")));
        let mut p = Parser::default();
        let got = p.push("{\"finding\":{\"severity\":\"risk\",\"title\":\"T\",\"file\":\"A.hs\",\"lines\":[3,5]}}\n");
        assert!(matches!(&got[0], Parsed::Finding(f) if f.lines == (3, 5) && f.severity == crate::review::Severity::Risk));
        let u = parse_line("{\"finding_update\":{\"id\":\"F3\",\"severity\":\"nit\",\"reason\":\"r\"}}");
        assert!(matches!(u, Some(Parsed::Update(u)) if u.id == "F3" && u.severity == Some(crate::review::Severity::Nit)));
        let w = parse_line("{\"finding_withdraw\":{\"id\":\"F3\",\"reason\":\"r\"}}");
        assert!(matches!(w, Some(Parsed::Withdraw(w)) if w.id == "F3"));
        let c = parse_line("{\"class\":{\"hunks\":[\"H1\",\"H2\"],\"kind\":\"docs\",\"reason\":\"r\"}}");
        assert!(matches!(c, Some(Parsed::Classes(cs)) if cs.len() == 2));
        let c = parse_line("{\"changes\":[\"a\",\"b\"]}");
        assert!(matches!(c, Some(Parsed::Changes(cs)) if cs == ["a", "b"]));
    }

    #[test]
    fn review_then_classify_then_plan() {
        use crate::classes::{Class, Kind};
        let ws = crate::classes::tests::ws();
        let tour = Tour {
            title: String::new(),
            intent: String::new(),
            range: None,
            glossary: Vec::new(),
            stages: Vec::new(),
        };
        let mut c = Classes::default();
        let next = |done, c: &mut Classes| match after(done, &ws, &tour, &[], c) {
            Next::Send(_, p) => Some(p),
            Next::Done => None,
        };
        assert_eq!(next(Phase::Review, &mut c), Some(Phase::Classify { retried: false }));
        c.accept(
            Class {
                hunk: "H1".into(),
                kind: Kind::Core,
                reason: String::new(),
            },
            &ws,
        );
        assert_eq!(next(Phase::Classify { retried: false }, &mut c), Some(Phase::Classify { retried: true }));
        assert_eq!(next(Phase::Classify { retried: true }, &mut c), Some(Phase::Plan));
        assert!(c.missing(&ws).is_empty());
        assert!(c.is_mechanical("H2"));
        assert_eq!(next(Phase::Plan, &mut c), None);
    }

    #[test]
    fn the_material_keeps_mechanical_hunks_with_a_hint() {
        use crate::diff::Hunk;
        let mut ws = crate::classes::tests::ws();
        ws.files[0].diff.hunks = ["H1", "H2"]
            .iter()
            .map(|id| Hunk {
                id: id.to_string(),
                old_start: 1,
                old_len: 1,
                new_start: 1,
                new_len: 1,
                lines: vec![format!("-old {id}"), format!("+new {id}")],
            })
            .collect();
        let m = material(&ws);
        assert!(m.contains("+new H1") && m.contains("+new H2"));
        assert!(m.contains("@@ H2 -1,1 +1,1  likely mechanical: import or export lines only"));
    }

    #[test]
    fn docs_hunks_leave_their_groups() {
        use crate::classes::{Class, Kind};
        let ws = crate::classes::tests::ws();
        let mut c = Classes::default();
        for (h, k) in [("H1", Kind::Docs), ("H2", Kind::Core), ("H3", Kind::Docs)] {
            c.accept(
                Class {
                    hunk: h.into(),
                    kind: k,
                    reason: String::new(),
                },
                &ws,
            );
        }
        let mut tour = Tour {
            title: String::new(),
            intent: String::new(),
            range: None,
            glossary: Vec::new(),
            stages: vec![Stage {
                title: "Docs and wiring".into(),
                steps: vec![tour::Step {
                    title: "s".into(),
                    text: String::new(),
                    blocks: vec![
                        Block::Lens(LensSpec::hunk("H3")),
                        Block::Group(tour::Group {
                            title: String::new(),
                            text: String::new(),
                            hunks: vec!["H1".into(), "H2".into(), "H3".into()],
                        }),
                    ],
                }],
            }],
        };
        complete(&mut tour, &ws, Some(&c));
        let b = &tour.stages[0].steps[0].blocks;
        assert_eq!(b.len(), 3);
        assert!(matches!(&b[1], Block::Lens(l) if l.hunk.as_deref() == Some("H1")));
        assert!(matches!(&b[2], Block::Group(g) if g.hunks == ["H2"]));
        assert_eq!(tour.stages.len(), 1, "nothing is left out");
    }

    #[test]
    fn the_mechanical_stage_follows_the_agent() {
        use crate::classes::{Class, Kind};
        let ws = crate::classes::tests::ws();
        let mut c = Classes::default();
        for (h, k, r) in [("H1", Kind::Mechanical, "rename"), ("H2", Kind::Core, ""), ("H3", Kind::Mechanical, "rename")] {
            c.accept(
                Class {
                    hunk: h.into(),
                    kind: k,
                    reason: r.into(),
                },
                &ws,
            );
        }
        let mut tour = Tour {
            title: String::new(),
            intent: String::new(),
            range: None,
            glossary: Vec::new(),
            stages: Vec::new(),
        };
        complete(&mut tour, &ws, Some(&c));
        let titles: Vec<&str> = tour.stages.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, vec!["Mechanical", LEFT_OUT]);
        assert!(matches!(&tour.stages[0].steps[0].blocks[0], Block::Group(g) if g.hunks == ["H1", "H3"]));
        assert!(matches!(&tour.stages[1].steps[0].blocks[0], Block::Group(g) if g.hunks == ["H2"]));
    }
}
