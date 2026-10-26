use crate::agent::{self, Agent, Event};
use crate::manifest::Workspace;
use std::collections::VecDeque;
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

pub const WHOLE: usize = 400_000;
const PART: usize = 150_000;
const PARALLEL: usize = 4;
const PREVIEW_LINES: usize = 6;

const HELPER_SYSTEM: &str = r#"You are one of several reviewers of a large code change. You review one part of it. Another session checks your findings and merges them with the other parts. You can read any file with Read, Grep, and Glob, also outside your part, when you need context."#;

fn hunk_text(h: &crate::diff::Hunk) -> String {
    h.lines.iter().map(|l| format!("{l}\n")).collect()
}

pub fn size(ws: &Workspace) -> usize {
    ws.files
        .iter()
        .flat_map(|f| &f.diff.hunks)
        .map(|h| h.lines.iter().map(|l| l.len() + 1).sum::<usize>())
        .sum()
}

/// Files keep their order, so a directory tends to stay in one part.
pub fn parts(ws: &Workspace) -> Option<Vec<Vec<String>>> {
    let (whole, part) = limits();
    if size(ws) <= whole {
        return None;
    }
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut used = 0;
    for f in &ws.files {
        let n: usize = f.diff.hunks.iter().map(|h| h.lines.iter().map(|l| l.len() + 1).sum::<usize>()).sum();
        if out.is_empty() || (used + n > part && used > 0) {
            out.push(Vec::new());
            used = 0;
        }
        out.last_mut().unwrap().push(f.diff.path.clone());
        used += n;
    }
    Some(out)
}

/// Debug builds read smaller limits from CICERONE_SPLIT, so split mode
/// can be tried on a small change.
fn limits() -> (usize, usize) {
    #[cfg(debug_assertions)]
    if let Some(n) = std::env::var("CICERONE_SPLIT").ok().and_then(|v| v.parse::<usize>().ok()) {
        return (n, n);
    }
    (WHOLE, PART)
}

fn part_prompt(ws: &Workspace, files: &[String], n: usize, total: usize) -> String {
    let mut s = format!(
        "You review part {n} of {total} of the change {}..{}.\n\n",
        ws.manifest.base, ws.manifest.head
    );
    if let Some(d) = &ws.snapshot {
        let _ = writeln!(
            s,
            "FILES\nThe full base and head versions of all changed files are at {0}/base/<path> and {0}/head/<path>.\n",
            d.display()
        );
    }
    let _ = writeln!(s, "ALL CHANGED FILES (for context)");
    for f in &ws.manifest.files {
        let _ = writeln!(s, "{} +{} -{}", f.path, f.added, f.removed);
    }
    let _ = writeln!(s, "\nDIFF OF YOUR PART");
    for f in ws.files.iter().filter(|f| files.contains(&f.diff.path)) {
        let _ = writeln!(s, "\n### {}", f.diff.path);
        for h in &f.diff.hunks {
            let _ = writeln!(s, "@@ {} -{},{} +{},{}", h.id, h.old_start, h.old_len, h.new_start, h.new_len);
            let text = hunk_text(h);
            if text.len() > PART {
                let _ = writeln!(s, "(this hunk is too long to show: read the file under FILES)");
            } else {
                s.push_str(&text);
            }
        }
    }
    let _ = writeln!(s, "\n{}", crate::review::prompt());
    s
}

pub fn merge_prompt(ws: &Workspace, parts: usize, findings: &[String], failed: &[String]) -> String {
    let mut s = format!(
        "REVIEW of the change {}..{}.\n\nThe change is too large for one session. {parts} helper sessions reviewed it in parts.\n\n",
        ws.manifest.base, ws.manifest.head
    );
    if let Some(d) = &ws.snapshot {
        let _ = writeln!(
            s,
            "FILES\nThe full base and head versions of the changed files are at {0}/base/<path> and {0}/head/<path>.\n",
            d.display()
        );
    }
    let _ = writeln!(s, "MANIFEST\n{}\n", serde_json::to_string(&ws.manifest).unwrap_or_default());
    let preview = |lines: usize| {
        let mut d = String::new();
        for f in &ws.files {
            let _ = writeln!(d, "\n### {}", f.diff.path);
            for h in &f.diff.hunks {
                let mech = ws
                    .hunk(&h.id)
                    .and_then(|x| x.mechanical.as_ref())
                    .map(|m| format!("  likely mechanical: {}", crate::classes::hint(ws, m)))
                    .unwrap_or_default();
                let _ = writeln!(d, "@@ {} -{},{} +{},{}{mech}", h.id, h.old_start, h.old_len, h.new_start, h.new_len);
                let changed: Vec<&String> = h.lines.iter().filter(|l| !l.starts_with(' ')).collect();
                for l in changed.iter().take(lines) {
                    let l: String = l.chars().take(160).collect();
                    let _ = writeln!(d, "{l}");
                }
                if changed.len() > lines {
                    let _ = writeln!(d, "… {} more changed lines", changed.len() - lines);
                }
            }
        }
        d
    };
    let mut diff = preview(PREVIEW_LINES);
    if diff.len() > WHOLE {
        diff = preview(0);
    }
    let _ = writeln!(
        s,
        "DIFF PREVIEW (the first changed lines of every hunk. Read the files under FILES for the rest)\n{diff}"
    );
    if !failed.is_empty() {
        let _ = writeln!(
            s,
            "\nThese parts got no review, because their helper failed. Review their files yourself: {}\n",
            failed.join(", ")
        );
    }
    let _ = writeln!(s, "\nHELPER FINDINGS");
    for f in findings {
        let _ = writeln!(s, "{f}");
    }
    let _ = writeln!(
        s,
        "\nCheck each helper finding against the code. Drop the wrong ones and the duplicates. Keep the right ones, with a better severity, text, or anchor where needed. Then add the findings that only show across parts: a caller in one part and a callee in another, a docs page and the code it describes, a version and the change it marks.\n\n{}",
        crate::review::prompt()
    );
    s
}

pub enum Msg {
    Finding(String),
    Tool(String),
    Done { part: usize, ok: bool },
}

pub struct Options {
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub files: Option<PathBuf>,
    pub tools: Option<PathBuf>,
}

pub fn start(ws: &Workspace, parts: &[Vec<String>], o: Options) -> Receiver<Msg> {
    let (tx, rx) = std::sync::mpsc::channel();
    let total = parts.len();
    let jobs: VecDeque<(usize, String)> = parts
        .iter()
        .enumerate()
        .map(|(i, files)| (i, part_prompt(ws, files, i + 1, total)))
        .collect();
    let jobs = Arc::new(Mutex::new(jobs));
    let o = Arc::new(o);
    for _ in 0..PARALLEL.min(total) {
        let jobs = Arc::clone(&jobs);
        let o = Arc::clone(&o);
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                let Some((i, prompt)) = jobs.lock().ok().and_then(|mut j| j.pop_front()) else {
                    return;
                };
                let ok = run_one(&o, &prompt, &tx);
                if tx.send(Msg::Done { part: i, ok }).is_err() {
                    return;
                }
            }
        });
    }
    rx
}

fn run_one(o: &Options, prompt: &str, out: &Sender<Msg>) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    let session = agent::new_session_id();
    let Ok(mut a) = Agent::start(
        agent::Options {
            cwd: &o.cwd,
            session: &session,
            resume: false,
            system: HELPER_SYSTEM,
            model: o.model.as_deref(),
            log: None,
            tools: o.tools.as_deref(),
            files: o.files.as_deref(),
        },
        tx,
    ) else {
        return false;
    };
    if a.send(prompt).is_err() {
        return false;
    }
    let mut parser = crate::plan::Parser::default();
    let forward = |items: Vec<crate::plan::Parsed>| {
        for it in items {
            if let crate::plan::Parsed::Finding(f) = it
                && let Ok(j) = serde_json::to_string(&serde_json::json!({ "finding": f }))
            {
                let _ = out.send(Msg::Finding(j));
            }
        }
    };
    for e in rx {
        match e {
            Event::Text(t) => forward(parser.push(&t)),
            Event::Tool(t) => {
                let _ = out.send(Msg::Tool(t));
            }
            Event::TurnDone { ok } => {
                forward(parser.finish());
                return ok;
            }
            Event::Exited(_) => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::Hunk;

    #[test]
    fn a_large_diff_splits_by_file_in_order() {
        let mut ws = crate::classes::tests::ws();
        assert!(parts(&ws).is_none());
        let big = |id: &str| Hunk {
            id: id.into(),
            old_start: 1,
            old_len: 1,
            new_start: 1,
            new_len: 1,
            lines: vec!["+".to_string() + &"x".repeat(PART - 10)],
        };
        let template = ws.files.remove(0);
        for (i, path) in ["a/1.hs", "a/2.hs", "b/3.hs", "b/4.hs"].iter().enumerate() {
            let mut f = crate::manifest::FileData {
                diff: template.diff.clone(),
                lang: None,
                base: None,
                head: None,
            };
            f.diff.path = path.to_string();
            f.diff.hunks = vec![big(&format!("H{i}"))];
            ws.files.push(f);
        }
        let p = parts(&ws).unwrap();
        assert_eq!(p.len(), 4);
        assert_eq!(p[0], vec!["a/1.hs"]);
        let m = merge_prompt(&ws, 4, &["{\"finding\":{}}".into()], &["part 2".into()]);
        assert!(m.contains("… ") || m.contains("+xxxx"));
        assert!(m.contains("HELPER FINDINGS\n{\"finding\":{}}"));
        assert!(m.contains("Review their files yourself: part 2"));
        assert!(m.len() < WHOLE + 50_000);
    }
}
