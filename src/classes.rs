use crate::manifest::{HunkEntry, Workspace};
use crate::mechanical::Mechanical;
use serde::{Deserialize, Serialize};
use std::fmt::Write;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Core,
    Supporting,
    Wiring,
    Docs,
    Tests,
    Mechanical,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::Core,
        Kind::Supporting,
        Kind::Wiring,
        Kind::Docs,
        Kind::Tests,
        Kind::Mechanical,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Core => "core",
            Kind::Supporting => "supporting",
            Kind::Wiring => "wiring",
            Kind::Docs => "docs",
            Kind::Tests => "tests",
            Kind::Mechanical => "mechanical",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Class {
    pub hunk: String,
    pub kind: Kind,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Line {
    #[serde(default)]
    pub hunk: Option<String>,
    #[serde(default)]
    pub hunks: Vec<String>,
    pub kind: Kind,
    #[serde(default)]
    pub reason: String,
}

impl Line {
    pub fn classes(self) -> Vec<Class> {
        self.hunk
            .into_iter()
            .chain(self.hunks)
            .map(|hunk| Class {
                hunk,
                kind: self.kind,
                reason: self.reason.clone(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Classes {
    #[serde(default)]
    pub changes: Vec<String>,
    #[serde(default)]
    pub hunks: Vec<Class>,
}

const NOT_CLASSIFIED: &str = "not classified by the agent";

pub const PROMPT: &str = r#"Now classify every hunk of the diff, from your review. Kinds:
- "core": the change that the PR is for.
- "supporting": new or changed code that the core change needs: types, helpers, queries.
- "wiring": code that connects the change to the rest: exports, routes, configuration, callers that only pass a value on.
- "docs": documentation, changelogs, release notes, API descriptions, comments that explain behavior.
- "tests": tests and test helpers.
- "mechanical": changes with no effect on behavior: formatting, import lists, renames done the same way everywhere, code moved with no change, generated files. A hint that says "likely mechanical" is not proof: check it, and do not use "mechanical" for a hunk that changes behavior in any way.

Answer with JSON Lines only. First one line that lists every distinct change in the diff, one sentence each, the main change first. Include the smaller changes too: a changed default, a docs page, a version bump, a behavior change in another part.
{"changes":["...","..."]}
Then one line per hunk, or one line for several hunks with the same kind and reason:
{"class":{"hunk":"H3","kind":"core","reason":"short: what the hunk does"}}
{"class":{"hunks":["H7","H8"],"kind":"mechanical","reason":"import lists only"}}
Every hunk id of the manifest must get exactly one class."#;

impl Classes {
    pub fn of(&self, hunk: &str) -> Option<&Class> {
        self.hunks.iter().find(|c| c.hunk == hunk)
    }

    pub fn kind(&self, hunk: &str) -> Option<Kind> {
        self.of(hunk).map(|c| c.kind)
    }

    pub fn is_mechanical(&self, hunk: &str) -> bool {
        self.kind(hunk) == Some(Kind::Mechanical)
    }

    pub fn accept(&mut self, mut c: Class, ws: &Workspace) -> bool {
        let Some(h) = ws.hunk(c.hunk.trim()) else {
            return false;
        };
        c.hunk = h.id.clone();
        self.hunks.retain(|x| x.hunk != c.hunk);
        self.hunks.push(c);
        true
    }

    pub fn missing<'a>(&self, ws: &'a Workspace) -> Vec<&'a HunkEntry> {
        ws.manifest.hunks.iter().filter(|h| self.of(&h.id).is_none()).collect()
    }

    pub fn fill_missing(&mut self, ws: &Workspace) {
        let missing: Vec<(String, Kind, String)> = self
            .missing(ws)
            .into_iter()
            .map(|h| match &h.mechanical {
                Some(m) => (h.id.clone(), Kind::Mechanical, format!("{NOT_CLASSIFIED}; {}", hint(ws, m))),
                None => (h.id.clone(), Kind::Supporting, NOT_CLASSIFIED.into()),
            })
            .collect();
        for (hunk, kind, reason) in missing {
            self.hunks.push(Class { hunk, kind, reason });
        }
        self.sort(ws);
    }

    fn sort(&mut self, ws: &Workspace) {
        let pos = |id: &str| ws.manifest.hunks.iter().position(|h| h.id == id).unwrap_or(usize::MAX);
        self.hunks.sort_by_key(|c| pos(&c.hunk));
    }

    pub fn coverage_note(&self, ws: &Workspace) -> String {
        let counts: Vec<String> = Kind::ALL
            .iter()
            .filter_map(|&k| {
                let n = self.count(k);
                (n > 0).then(|| format!("{n} {}", k.label()))
            })
            .collect();
        let mut s = format!(
            "**Coverage.** The review covered all {} hunks in {} files: {}.",
            ws.manifest.hunks.len(),
            ws.manifest.files.len(),
            counts.join(", ")
        );
        let guessed = self.hunks.iter().filter(|c| c.reason.starts_with(NOT_CLASSIFIED)).count();
        if guessed > 0 {
            let _ = write!(s, " The agent gave no class to {guessed} of them; they use the class of a syntax detector.");
        }
        let groups = self.mechanical_groups();
        if !groups.is_empty() {
            s.push_str("\n\nMechanical hunks, checked to have no effect on behavior:");
            for (reason, hunks) in groups {
                let mut files: Vec<&str> = Vec::new();
                for f in hunks.iter().filter_map(|h| ws.hunk(h)).map(|h| h.file.as_str()) {
                    if !files.contains(&f) {
                        files.push(f);
                    }
                }
                let shown: Vec<String> = files.iter().take(4).map(|f| format!("`{}`", crate::tour::short(f))).collect();
                let more = if files.len() > 4 { format!(" and {} more files", files.len() - 4) } else { String::new() };
                let reason = if reason.is_empty() { "mechanical" } else { reason.as_str() };
                let n = hunks.len();
                let _ = write!(s, "\n- {reason}: {n} hunk{} in {}{more}", if n == 1 { "" } else { "s" }, shown.join(", "));
            }
        }
        s
    }

    pub fn changes_list(&self) -> String {
        self.changes.iter().map(|c| format!("- {}\n", c.trim())).collect()
    }

    pub fn count(&self, kind: Kind) -> usize {
        self.hunks.iter().filter(|c| c.kind == kind).count()
    }

    pub fn mechanical_groups(&self) -> Vec<(String, Vec<String>)> {
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        for c in self.hunks.iter().filter(|c| c.kind == Kind::Mechanical) {
            match out.iter_mut().find(|(r, _)| *r == c.reason) {
                Some((_, v)) => v.push(c.hunk.clone()),
                None => out.push((c.reason.clone(), vec![c.hunk.clone()])),
            }
        }
        out
    }

    pub fn load(cache: Option<&Path>, ws: &Workspace) -> Option<Classes> {
        let j = std::fs::read_to_string(cache?.join("classes.json")).ok()?;
        let mut c: Classes = serde_json::from_str(&j).ok()?;
        c.hunks.retain(|x| ws.hunk(&x.hunk).is_some());
        Some(c)
    }

    pub fn save(&self, cache: Option<&Path>) {
        if let Some(d) = cache
            && std::fs::create_dir_all(d).is_ok()
            && let Ok(j) = serde_json::to_string_pretty(self)
        {
            let _ = std::fs::write(d.join("classes.json"), j);
        }
    }
}

pub fn retry_prompt(missing: &[&HunkEntry]) -> String {
    let mut s = format!(
        "These {} hunks have no class yet. Classify each of them, in the same format. Their diff is in the first message.\n",
        missing.len()
    );
    for h in missing {
        let _ = writeln!(s, "{} {}:{} +{} -{}", h.id, h.file, h.new.0, h.added, h.removed);
    }
    s
}

pub fn hint(ws: &Workspace, m: &Mechanical) -> String {
    match m {
        Mechanical::Generated => "generated file".into(),
        Mechanical::Formatting => "whitespace and layout only".into(),
        Mechanical::Imports => "import or export lines only".into(),
        Mechanical::Pattern(id) => {
            let subs: Vec<String> = ws
                .manifest
                .patterns
                .iter()
                .find(|p| &p.id == id)
                .map(|p| p.substitutions.iter().map(|s| format!("`{}` → `{}`", s.old, s.new)).collect())
                .unwrap_or_default();
            format!("same substitution as the other hunks of pattern {id}: {}", subs.join(", "))
        }
        Mechanical::Moved(others) if others.is_empty() => "code moved inside this hunk, with no other change".into(),
        Mechanical::Moved(others) => format!("moved code: the same lines, up to whitespace, as {}", places(ws, others)),
    }
}

fn places(ws: &Workspace, hunks: &[String]) -> String {
    let ps: Vec<String> = hunks
        .iter()
        .map(|o| match ws.hunk(o) {
            Some(h) => format!("{o} in `{}`", h.file),
            None => o.clone(),
        })
        .collect();
    ps.join(", ")
}

/// The detector hints for a hunk, to append to its `@@` line.
pub fn hints(ws: &Workspace, id: &str) -> String {
    let Some(h) = ws.hunk(id) else {
        return String::new();
    };
    match &h.mechanical {
        Some(m) => format!("  likely mechanical: {}", hint(ws, m)),
        None if !h.partly_moved.is_empty() => format!(
            "  partly moved: some lines are the same as {}, the other lines are new",
            places(ws, &h.partly_moved)
        ),
        None => String::new(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::diff::{FileDiff, FileStatus};
    use crate::manifest::{FileData, Manifest, Place};

    fn entry(id: &str, mechanical: Option<Mechanical>) -> HunkEntry {
        HunkEntry {
            id: id.into(),
            file: "A.hs".into(),
            old: (1, 1),
            new: (1, 1),
            added: 1,
            removed: 1,
            place: Place::Other,
            mechanical,
            partly_moved: Vec::new(),
            symbols: Vec::new(),
        }
    }

    pub(crate) fn ws() -> Workspace {
        Workspace {
            manifest: Manifest {
                base: "a".into(),
                head: "b".into(),
                files: Vec::new(),
                symbols: Vec::new(),
                hunks: vec![entry("H1", None), entry("H2", Some(Mechanical::Imports)), entry("H3", None)],
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
                base: None,
                head: None,
            }],
            snapshot: None,
        }
    }

    #[test]
    fn hints_name_where_code_moved() {
        let mut ws = ws();
        ws.manifest.hunks[0].mechanical = Some(Mechanical::Moved(vec!["H3".into()]));
        ws.manifest.hunks[2].partly_moved = vec!["H1".into()];
        assert_eq!(
            hints(&ws, "H1"),
            "  likely mechanical: moved code: the same lines, up to whitespace, as H3 in `A.hs`"
        );
        assert_eq!(
            hints(&ws, "H3"),
            "  partly moved: some lines are the same as H1 in `A.hs`, the other lines are new"
        );
    }

    #[test]
    fn a_line_can_classify_several_hunks() {
        let l: Line = serde_json::from_str(r#"{"hunks":["H1","H2"],"kind":"mechanical","reason":"imports"}"#).unwrap();
        let cs = l.classes();
        assert_eq!(cs.len(), 2);
        assert!(cs.iter().all(|c| c.kind == Kind::Mechanical && c.reason == "imports"));
    }

    #[test]
    fn every_hunk_ends_with_one_class() {
        let ws = ws();
        let mut c = Classes::default();
        let class = |hunk: &str, kind| Class {
            hunk: hunk.into(),
            kind,
            reason: String::new(),
        };
        assert!(c.accept(class("H3", Kind::Mechanical), &ws));
        assert!(c.accept(class("H3", Kind::Core), &ws));
        assert!(!c.accept(class("H9", Kind::Core), &ws));
        let missing: Vec<&str> = c.missing(&ws).iter().map(|h| h.id.as_str()).collect();
        assert_eq!(missing, vec!["H1", "H2"]);
        c.fill_missing(&ws);
        let got: Vec<(&str, Kind)> = c.hunks.iter().map(|x| (x.hunk.as_str(), x.kind)).collect();
        assert_eq!(got, vec![("H1", Kind::Supporting), ("H2", Kind::Mechanical), ("H3", Kind::Core)]);
        let note = c.coverage_note(&ws);
        assert!(note.contains("all 3 hunks in 0 files: 1 core, 1 supporting, 1 mechanical."), "{note}");
        assert!(note.contains("no class to 2 of them"));
        assert!(note.contains("- not classified by the agent; import or export lines only: 1 hunk in `A.hs`"), "{note}");
    }
}
