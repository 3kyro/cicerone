use crate::lens::LensSpec;
use crate::manifest::Workspace;
use crate::tour::{Block, Stage, Step, Tour};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Bug,
    Risk,
    Question,
    Nit,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Bug => "BUG",
            Severity::Risk => "RISK",
            Severity::Question => "QUESTION",
            Severity::Nit => "NIT",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    #[serde(default)]
    pub id: String,
    pub severity: Severity,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub file: String,
    pub lines: (usize, usize),
    #[serde(default)]
    pub withdrawn: bool,
    /// Earlier states, oldest first, each with the reason it changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Revision>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Revision {
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub lines: (usize, usize),
    pub withdrawn: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Update {
    pub id: String,
    #[serde(default)]
    pub severity: Option<Severity>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub lines: Option<(usize, usize)>,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Withdraw {
    pub id: String,
    #[serde(default)]
    pub reason: String,
}

impl Finding {
    fn snapshot(&self, reason: &str) -> Revision {
        Revision {
            severity: self.severity,
            title: self.title.clone(),
            body: self.body.clone(),
            lines: self.lines,
            withdrawn: self.withdrawn,
            reason: reason.to_string(),
        }
    }

    /// Applies `u` and records the previous state. Returns a one-line
    /// summary of the change, or `None` when nothing changed.
    pub fn revise(&mut self, u: Update) -> Option<String> {
        let before = self.snapshot(&u.reason);
        let mut what = Vec::new();
        if let Some(s) = u.severity
            && s != self.severity
        {
            what.push(format!("{} → {}", self.severity.label(), s.label()));
            self.severity = s;
        }
        if let Some(t) = u.title.filter(|t| *t != self.title) {
            self.title = t;
            what.push("title".into());
        }
        if let Some(b) = u.body.filter(|b| *b != self.body) {
            self.body = b;
            what.push("text".into());
        }
        if let Some(l) = u.lines.filter(|l| *l != self.lines) {
            self.lines = (l.0.min(l.1).max(1), l.0.max(l.1).max(1));
            what.push("lines".into());
        }
        if self.withdrawn {
            self.withdrawn = false;
            what.push("reopened".into());
        }
        if what.is_empty() {
            return None;
        }
        self.history.push(before);
        Some(what.join(", "))
    }

    pub fn withdraw(&mut self, reason: &str) -> bool {
        if self.withdrawn {
            return false;
        }
        self.history.push(self.snapshot(reason));
        self.withdrawn = true;
        true
    }

    pub fn hits(&self, file: &str, a: usize, b: usize) -> bool {
        self.file == file && self.lines.0 <= b && a <= self.lines.1
    }
}

pub const TITLE: &str = "Review";

/// Sent with each chat question, because a resumed session keeps the system
/// prompt it started with.
pub const UPDATE_RULES: &str = r#"If your answer shows that a finding is wrong, too severe, too mild, or needs a better text or anchor, also change it with one JSON object on its own line: {"finding_update":{"id":"F3","severity":"nit","title":"...","body":"...","lines":[a,b],"reason":"why"}} (only the fields that change, always a reason), or {"finding_withdraw":{"id":"F3","reason":"why"}}, or {"finding":{...}} for a new problem. Say the change in the text too. No other JSON."#;
const CONTEXT: usize = 3;

pub const PROMPT: &str = r#"Now review the change as a careful senior reviewer. Report what a reviewer must act on or ask about: bugs, behavior changes that look unintended, wrong or missing error handling, security and data handling, concurrency, and missing tests for risky paths. Skip style and formatting.

Answer with JSON Lines only, one line per finding, each as soon as you have it:
{"finding":{"severity":"bug|risk|question|nit","title":"short","body":"markdown: what is wrong, why it matters, what to do","file":"path","lines":[a,b]}}
"lines" are head line numbers in that file and must point at the exact code. For removed code, use the head lines where it was. At most 12 findings, most severe first. Check each claim against the code before you report it. If you find nothing, answer with nothing."#;

/// Keeps a finding only when it points into a changed file at lines that
/// exist, and gives it an id.
pub fn accept(mut f: Finding, ws: &Workspace, n: usize) -> Option<Finding> {
    let file = ws.file(&f.file)?;
    let len = file.head.as_deref().or(file.base.as_deref())?.lines().count().max(1);
    let (a, b) = (f.lines.0.min(f.lines.1).max(1), f.lines.0.max(f.lines.1).max(1));
    if a > len {
        return None;
    }
    f.lines = (a, b.min(len));
    f.id = format!("F{n}");
    Some(f)
}

pub fn load(cache: Option<&Path>) -> Option<Vec<Finding>> {
    let j = std::fs::read_to_string(cache?.join("review.json")).ok()?;
    serde_json::from_str(&j).ok()
}

pub fn save(findings: &[Finding], cache: Option<&Path>) {
    if let Some(d) = cache
        && let Ok(j) = serde_json::to_string_pretty(findings)
    {
        let _ = std::fs::write(d.join("review.json"), j);
    }
}

/// Replaces the review stage of `tour` with one step per finding, most
/// severe first.
pub fn sync_stage(tour: &mut Tour, findings: &[Finding]) {
    tour.stages.retain(|s| s.title != TITLE);
    if findings.is_empty() {
        return;
    }
    let mut sorted: Vec<&Finding> = findings.iter().collect();
    sorted.sort_by_key(|f| (f.withdrawn, f.severity));
    tour.stages.push(Stage {
        title: TITLE.into(),
        steps: sorted
            .into_iter()
            .map(|f| Step {
                title: if f.withdrawn { format!("withdrawn: {}", f.title) } else { f.title.clone() },
                text: String::new(),
                blocks: vec![
                    Block::Finding(f.id.clone()),
                    Block::Lens(LensSpec {
                        focus: vec![f.lines],
                        ..LensSpec::lines(&f.file, f.lines.0.saturating_sub(CONTEXT).max(1), f.lines.1 + CONTEXT)
                    }),
                ],
            })
            .collect(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{FileDiff, FileStatus};
    use crate::manifest::{FileData, Manifest};

    fn ws() -> Workspace {
        Workspace {
            manifest: Manifest {
                base: "a".into(),
                head: "b".into(),
                files: Vec::new(),
                symbols: Vec::new(),
                hunks: Vec::new(),
                patterns: Vec::new(),
                order: Vec::new(),
            },
            files: vec![FileData {
                diff: FileDiff {
                    path: "A.hs".into(),
                    old_path: None,
                    status: FileStatus::Modified,
                    binary: false,
                    hunks: Vec::new(),
                },
                lang: None,
                base: Some("x\n".into()),
                head: Some("a\nb\nc\nd\n".into()),
            }],
            snapshot: None,
        }
    }

    fn finding(sev: Severity, file: &str, lines: (usize, usize)) -> Finding {
        Finding {
            id: String::new(),
            severity: sev,
            title: "t".into(),
            body: String::new(),
            file: file.into(),
            lines,
            withdrawn: false,
            history: Vec::new(),
        }
    }

    #[test]
    fn revise_keeps_the_previous_state() {
        let mut f = finding(Severity::Risk, "A.hs", (1, 2));
        let u = Update {
            id: "F1".into(),
            severity: Some(Severity::Nit),
            title: None,
            body: None,
            lines: None,
            reason: "the path holds no personal data".into(),
        };
        assert_eq!(f.revise(u).as_deref(), Some("RISK → NIT"));
        assert_eq!(f.history.len(), 1);
        assert_eq!(f.history[0].severity, Severity::Risk);
        assert!(f.withdraw("covered elsewhere"));
        assert!(f.withdrawn && f.history.len() == 2);
        assert!(!f.withdraw("again"));
    }

    #[test]
    fn accept_checks_file_and_lines() {
        let ws = ws();
        let f = accept(finding(Severity::Bug, "A.hs", (9, 3)), &ws, 1).unwrap();
        assert_eq!((f.id.as_str(), f.lines), ("F1", (3, 4)));
        assert!(accept(finding(Severity::Bug, "B.hs", (1, 1)), &ws, 2).is_none());
        assert!(accept(finding(Severity::Bug, "A.hs", (7, 8)), &ws, 3).is_none());
    }

    #[test]
    fn review_stage_puts_bugs_first() {
        let mut t = Tour {
            title: String::new(),
            intent: String::new(),
            range: None,
            glossary: Vec::new(),
            stages: Vec::new(),
        };
        let mut nit = finding(Severity::Nit, "A.hs", (1, 1));
        nit.id = "F1".into();
        let mut bug = finding(Severity::Bug, "A.hs", (2, 2));
        bug.id = "F2".into();
        let all = [nit, bug];
        sync_stage(&mut t, &all);
        sync_stage(&mut t, &all);
        assert_eq!(t.stages.len(), 1);
        assert!(matches!(&t.stages[0].steps[0].blocks[0], Block::Finding(id) if id == "F2"));
    }
}
