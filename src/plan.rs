use crate::manifest::Workspace;
use crate::tour::{self, Block, Gloss, Stage, Tour};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fmt::Write;
use std::path::PathBuf;

pub const SYSTEM: &str = r#"You are the guide inside cicerone, a terminal tool that walks a human reviewer through a code change in stages. First you plan the tour. Later the reviewer chats with you about the change while reading it.

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
6. Leave out mechanical hunks and patterns: cicerone appends a mechanical stage. Every other hunk must appear in the tour, in a lens or in a group.
   When many hunks make the same kind of change (for example, every handler moves to a new API), show one or two of them as lenses, then put all of them in one group in the same step, with a text that names the shared change and any site that differs.
7. Zoom, do not dump. This is the point of the tool:
   - Prefer symbol lenses. Use a hunk lens only for a hunk outside every symbol (imports, exports, non-code files).
   - In the shapes stage, show functions at "signature" and changed types at "signature". Show a new type at "full" only when it is short.
   - Show a function longer than 15 lines first at "skeleton", with a label on each hole that hides something the reader must know. Show it at "full" only in a later step, with "focus" on the lines that matter.
   - Use "notes" for the one to three lines in a step that a reviewer must not miss.
8. To read more context, use Read, Grep, and Glob on the FILES directory named in the material, and on the repository working tree. The working tree can differ from head.
9. 4 to 8 stages. 1 to 4 lenses per step.

COVERAGE
After the plan, cicerone can send the hunks that the tour does not show yet. Answer with JSON Lines only: one or more {"stage":{...}} lines whose steps sort those hunks into "group" blocks by what they do. Each listed hunk goes into exactly one group. Use lenses only for a hunk that a reviewer must read in full.

CHAT
After the plan, the reviewer can send questions. Each question names the step and the lens they are looking at. Answer in short markdown for a terminal: plain sentences, `code`, bullets. No headings, no tables, no JSON."#;

const MAX_DIFF: usize = 400_000;

pub fn prompt(ws: &Workspace) -> String {
    format!("Plan the tour for the change {}..{}.\n\n{}", ws.manifest.base, ws.manifest.head, material(ws))
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
    let _ = writeln!(s, "DIFF (hunks marked mechanical in the manifest are left out)");
    let mut left_out = 0;
    for f in &ws.files {
        let mut header = false;
        for h in &f.diff.hunks {
            if ws.hunk(&h.id).is_some_and(|x| x.mechanical.is_some()) {
                continue;
            }
            let text: String = h.lines.iter().map(|l| format!("{l}\n")).collect();
            if s.len() + text.len() > MAX_DIFF {
                left_out += 1;
                continue;
            }
            if !header {
                let _ = writeln!(s, "\n### {}", f.diff.path);
                header = true;
            }
            let _ = writeln!(
                s,
                "@@ {} -{},{} +{},{}",
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
#[serde(rename_all = "lowercase")]
enum Item {
    Tour(Header),
    Stage(Stage),
    Finding(crate::review::Finding),
}

#[derive(Default)]
pub struct Parser {
    buf: String,
}

pub enum Parsed {
    Header { title: String, intent: String, glossary: Vec<Gloss> },
    Stage(Stage),
    Finding(crate::review::Finding),
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

fn parse_line(line: &str) -> Option<Parsed> {
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
    }
}

pub const LEFT_OUT: &str = "Left out by the tour";

/// Hunks that are not mechanical and that no lens or group of the tour shows.
pub fn uncovered<'a>(tour: &Tour, ws: &'a Workspace) -> Vec<&'a crate::manifest::HunkEntry> {
    let shown = shown_hunks(tour, ws);
    ws.manifest
        .hunks
        .iter()
        .filter(|h| h.mechanical.is_none() && !shown.contains(h.id.as_str()))
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
pub fn refresh(tour: &mut Tour, ws: &Workspace) {
    tour.stages.retain(|s| {
        s.title != "Mechanical" && s.title != LEFT_OUT && s.title != "Not covered yet" && s.title != crate::review::TITLE
    });
    complete(tour, ws);
}

/// Appends the mechanical stage and a stage for every hunk that the tour
/// still does not show, so nothing escapes the review.
pub fn complete(tour: &mut Tour, ws: &Workspace) {
    let auto = tour::auto(ws);
    if let Some(mech) = auto.stages.iter().find(|s| s.title == "Mechanical")
        && !tour.stages.iter().any(|s| s.title == "Mechanical")
    {
        tour.stages.push(mech.clone());
    }
    let mut missing: Vec<(&str, Vec<String>)> = Vec::new();
    for h in uncovered(tour, ws) {
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
                .map(|(file, hunks)| tour::Step {
                    title: tour::short(file),
                    text: String::new(),
                    blocks: vec![Block::Group(tour::Group {
                        title: String::new(),
                        text: String::new(),
                        hunks,
                    })],
                })
                .collect(),
        });
    }
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
    let apply = |items: Vec<Parsed>, tour: &mut Tour, findings: &mut Vec<review::Finding>| {
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
                Parsed::Finding(f) => {
                    if let Some(f) = review::accept(f, ws, findings.len() + 1) {
                        eprintln!("finding: {} {}", f.severity.label(), crate::clean(&f.title));
                        findings.push(f);
                    }
                }
            }
        }
    };
    #[derive(PartialEq)]
    enum Phase {
        Plan,
        Cover,
        Review,
    }
    let mut phase = Phase::Plan;
    for e in rx {
        match e {
            Event::Text(t) => apply(parser.push(&t), &mut tour, &mut findings),
            Event::Tool(t) => eprintln!("tool: {}", crate::clean(&t)),
            Event::TurnDone { ok } => {
                apply(parser.finish(), &mut tour, &mut findings);
                parser = Parser::default();
                anyhow::ensure!(ok, "the agent reported an error");
                anyhow::ensure!(!tour.stages.is_empty(), "the agent returned no stages");
                if phase == Phase::Review {
                    break;
                }
                let missing = uncovered(&tour, ws);
                if phase == Phase::Plan && !missing.is_empty() {
                    eprintln!("coverage: {} hunks", missing.len());
                    agent.send(&coverage_prompt(ws, &missing))?;
                    phase = Phase::Cover;
                    continue;
                }
                complete(&mut tour, ws);
                save(&tour, cache);
                eprintln!("reviewing");
                agent.send(review::PROMPT)?;
                phase = Phase::Review;
            }
            Event::Exited(err) => anyhow::bail!("the agent stopped: {err}"),
        }
    }
    review::save(&findings, cache);
    review::sync_stage(&mut tour, &findings);
    Ok(tour)
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
    }
}
