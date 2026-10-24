use crate::highlight::{self, Hl, Spans};
use crate::manifest::{Change, FileData, Symbol, Workspace};
use crate::syntax::{self, DeclKind, Outline};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Resolution {
    Name,
    Signature,
    Skeleton,
    Full,
}

impl Resolution {
    pub fn label(self) -> &'static str {
        match self {
            Resolution::Name => "name",
            Resolution::Signature => "sig",
            Resolution::Skeleton => "skeleton",
            Resolution::Full => "full",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoleLabel {
    pub lines: (usize, usize),
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub line: usize,
    pub text: String,
}

fn full() -> Resolution {
    Resolution::Full
}

/// What a lens looks at. Exactly one of `symbol`, `hunk`, or `file` with
/// `lines` names the target. Line numbers are head lines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LensSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hunk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<(usize, usize)>,
    #[serde(default = "full")]
    pub resolution: Resolution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holes: Vec<HoleLabel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focus: Vec<(usize, usize)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
}

impl LensSpec {
    pub fn symbol(id: &str, resolution: Resolution) -> LensSpec {
        LensSpec {
            symbol: Some(id.into()),
            hunk: None,
            file: None,
            lines: None,
            resolution,
            diff: None,
            holes: Vec::new(),
            focus: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn hunk(id: &str) -> LensSpec {
        LensSpec {
            symbol: None,
            hunk: Some(id.into()),
            ..LensSpec::symbol("", Resolution::Full)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Line {
    Ctx,
    Add,
    Del,
}

#[derive(Debug, Clone)]
struct URow {
    kind: Line,
    old: Option<usize>,
    new: Option<usize>,
    text: String,
    hunk: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    Ctx,
    Add,
    Del,
    Hole {
        label: String,
        count: usize,
        changed: bool,
    },
}

#[derive(Debug, Clone)]
pub struct Row {
    pub kind: RowKind,
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub text: String,
    pub spans: Vec<(usize, usize, Hl)>,
    pub focus: bool,
    pub note: Option<String>,
}

pub struct LensView {
    pub title: String,
    pub place: String,
    pub resolution: Resolution,
    pub available: Vec<Resolution>,
    pub diff: bool,
    pub rows: Vec<Row>,
}

struct FileView {
    rows: Vec<URow>,
    tree: Option<tree_sitter::Tree>,
    head_hl: Spans,
    base_hl: Spans,
    head: Outline,
    base: Outline,
}

#[derive(Default)]
pub struct Lenses {
    files: HashMap<String, FileView>,
}

impl Lenses {
    fn file<'a>(&'a mut self, f: &FileData) -> &'a FileView {
        self.files
            .entry(f.diff.path.clone())
            .or_insert_with(|| build_file(f))
    }

    pub fn available(ws: &Workspace, spec: &LensSpec) -> Vec<Resolution> {
        match spec.symbol.as_deref().and_then(|id| ws.symbol(id)) {
            Some(s) if s.change == Change::Removed => vec![Resolution::Name, Resolution::Full],
            Some(s) if s.kind.is_type_level() && s.change == Change::Added => {
                vec![Resolution::Name, Resolution::Full]
            }
            Some(s) if s.kind.is_type_level() => vec![Resolution::Name, Resolution::Signature, Resolution::Full],
            Some(_) => vec![
                Resolution::Name,
                Resolution::Signature,
                Resolution::Skeleton,
                Resolution::Full,
            ],
            None => vec![Resolution::Full],
        }
    }

    pub fn view(&mut self, ws: &Workspace, spec: &LensSpec, res: Resolution, diff: Option<bool>) -> LensView {
        let available = Lenses::available(ws, spec);
        let res = if available.contains(&res) {
            res
        } else {
            *available.last().unwrap()
        };
        if let Some(sym) = spec.symbol.as_deref().and_then(|id| ws.symbol(id)) {
            return self.symbol_view(ws, spec, sym, res, diff, available);
        }
        let (file, title, pick): (Option<&FileData>, String, Box<dyn Fn(&URow, usize) -> bool>) =
            if let Some(h) = spec.hunk.as_deref().and_then(|id| ws.hunk(id)) {
                let f = ws.file(&h.file);
                let idx = f.and_then(|f| f.diff.hunks.iter().position(|x| x.id == h.id));
                (f, h.id.clone(), Box::new(move |r: &URow, _| r.hunk.is_some() && r.hunk == idx))
            } else if let (Some(path), Some((a, b))) = (spec.file.as_deref(), spec.lines) {
                (
                    ws.file(path),
                    format!("{a}-{b}"),
                    Box::new(move |r: &URow, _| r.new.is_some_and(|n| a <= n && n <= b)),
                )
            } else {
                (None, String::new(), Box::new(|_: &URow, _| false))
            };
        let Some(file) = file else {
            return empty(spec, available);
        };
        let diff = diff.or(spec.diff).unwrap_or(true);
        let fv = self.file(file);
        let picked: Vec<&URow> = fv.rows.iter().enumerate().filter(|(i, r)| pick(r, *i)).map(|(_, r)| r).collect();
        let rows = fold(fv, &picked, |_| true, diff, spec);
        LensView {
            title,
            place: place(&file.diff.path, &picked),
            resolution: Resolution::Full,
            available,
            diff,
            rows,
        }
    }

    fn symbol_view(
        &mut self,
        ws: &Workspace,
        spec: &LensSpec,
        sym: &Symbol,
        res: Resolution,
        diff: Option<bool>,
        available: Vec<Resolution>,
    ) -> LensView {
        let Some(file) = ws.file(&sym.file) else {
            return empty(spec, available);
        };
        let diff = diff
            .or(spec.diff)
            .unwrap_or(!matches!(sym.change, Change::Added));
        let lang = file.lang;
        let fv = self.file(file);
        let removed = sym.change == Change::Removed;
        let (s, e) = sym.lines;
        let head_decl = (!removed)
            .then(|| fv.head.decls.iter().find(|d| d.start == s && d.name == sym.name))
            .flatten();
        let base_decl = if removed {
            fv.base.decls.iter().find(|d| d.start == s && d.name == sym.name)
        } else {
            fv.base
                .decls
                .iter()
                .find(|d| d.name == sym.name && d.kind == sym.kind)
        };
        let base_range = base_decl.map(|d| (d.start, d.end));
        let picked: Vec<&URow> = fv
            .rows
            .iter()
            .filter(|r| {
                if removed {
                    r.old.is_some_and(|o| s <= o && o <= e) && r.kind != Line::Add
                } else {
                    r.new.is_some_and(|n| s <= n && n <= e)
                        || (r.kind == Line::Del
                            && base_range.is_some_and(|(bs, be)| r.old.is_some_and(|o| bs <= o && o <= be)))
                }
            })
            .collect();

        let title = sym.name.clone();
        let place = place(&sym.file, &picked);
        if res == Resolution::Name {
            let text = sym
                .signature
                .as_deref()
                .map(syntax::normalize)
                .unwrap_or_else(|| {
                    picked
                        .iter()
                        .find(|r| r.kind != Line::Del)
                        .map(|r| r.text.trim().to_string())
                        .unwrap_or_default()
                });
            let row = Row {
                kind: RowKind::Ctx,
                old: None,
                new: Some(s),
                text,
                spans: Vec::new(),
                focus: false,
                note: None,
            };
            return LensView {
                title,
                place,
                resolution: res,
                available,
                diff,
                rows: vec![row],
            };
        }

        let keep: Box<dyn Fn(&URow) -> bool> = match res {
            Resolution::Signature if sym.kind.is_type_level() => {
                let changed: BTreeSet<usize> = picked
                    .iter()
                    .filter(|r| r.kind != Line::Ctx)
                    .filter_map(|r| r.new.or(r.old))
                    .collect();
                let first = head_decl.map(|d| d.start);
                let near = move |n: usize| changed.iter().any(|&c| c.abs_diff(n) <= 1);
                Box::new(move |r: &URow| {
                    r.kind != Line::Ctx || r.new == first || r.new.is_some_and(&near)
                })
            }
            Resolution::Signature => {
                let hs = head_decl.and_then(|d| d.sig_lines);
                let bs = base_decl.and_then(|d| d.sig_lines);
                let first = head_decl.map(|d| d.start);
                Box::new(move |r: &URow| match r.kind {
                    Line::Del => bs.is_some_and(|(a, b)| r.old.is_some_and(|o| a <= o && o <= b)),
                    _ => match hs {
                        Some((a, b)) => r.new.is_some_and(|n| a <= n && n <= b),
                        None => r.new == first,
                    },
                })
            }
            Resolution::Skeleton => {
                let anchors: BTreeSet<usize> = match (lang, &fv.tree) {
                    (Some(l), Some(t)) => syntax::anchors(l, t, s, e),
                    _ => BTreeSet::new(),
                };
                let sig = head_decl.and_then(|d| d.sig_lines);
                Box::new(move |r: &URow| {
                    r.kind != Line::Del
                        && r.new.is_some_and(|n| {
                            anchors.contains(&n) || sig.is_some_and(|(a, b)| a <= n && n <= b)
                        })
                })
            }
            _ => Box::new(|_: &URow| true),
        };
        let rows = fold(fv, &picked, keep, diff, spec);
        LensView {
            title,
            place,
            resolution: res,
            available,
            diff,
            rows,
        }
    }
}

fn empty(spec: &LensSpec, available: Vec<Resolution>) -> LensView {
    LensView {
        title: spec
            .symbol
            .clone()
            .or(spec.hunk.clone())
            .unwrap_or_else(|| "?".into()),
        place: "not found".into(),
        resolution: Resolution::Full,
        available,
        diff: false,
        rows: Vec::new(),
    }
}

fn place(path: &str, rows: &[&URow]) -> String {
    let lines: Vec<usize> = rows.iter().filter_map(|r| r.new.or(r.old)).collect();
    match (lines.iter().min(), lines.iter().max()) {
        (Some(a), Some(b)) if a != b => format!("{path}:{a}-{b}"),
        (Some(a), _) => format!("{path}:{a}"),
        _ => path.to_string(),
    }
}

const MIN_HOLE: usize = 1;

fn fold(
    fv: &FileView,
    picked: &[&URow],
    keep: impl Fn(&URow) -> bool,
    diff: bool,
    spec: &LensSpec,
) -> Vec<Row> {
    let all_new = picked.iter().all(|r| r.kind == Line::Add);
    let row = |r: &URow| -> Row {
        let spans = match r.kind {
            Line::Del => r.old.and_then(|o| fv.base_hl.get(o - 1)),
            _ => r.new.and_then(|n| fv.head_hl.get(n - 1)),
        }
        .cloned()
        .unwrap_or_default();
        let focus = r
            .new
            .is_some_and(|n| spec.focus.iter().any(|&(a, b)| a <= n && n <= b));
        let note = r
            .new
            .filter(|_| r.kind != Line::Del)
            .and_then(|n| spec.notes.iter().find(|x| x.line == n))
            .map(|x| x.text.clone());
        Row {
            kind: match r.kind {
                Line::Ctx => RowKind::Ctx,
                Line::Add => RowKind::Add,
                Line::Del => RowKind::Del,
            },
            old: r.old,
            new: r.new,
            text: r.text.clone(),
            spans,
            focus,
            note,
        }
    };
    let mut out: Vec<Row> = Vec::new();
    let mut hidden: Vec<&URow> = Vec::new();
    let flush = |hidden: &mut Vec<&URow>, out: &mut Vec<Row>| {
        if hidden.is_empty() {
            return;
        }
        let visible: Vec<&&URow> = hidden.iter().filter(|r| diff || r.kind != Line::Del).collect();
        if visible.len() <= MIN_HOLE {
            out.extend(visible.into_iter().map(|r| row(r)));
            hidden.clear();
            return;
        }
        let shown: Vec<usize> = hidden.iter().filter(|r| r.kind != Line::Del).filter_map(|r| r.new).collect();
        let count = shown.len().max(hidden.iter().filter(|r| r.kind == Line::Del).count());
        let changed = !all_new && hidden.iter().any(|r| r.kind != Line::Ctx);
        let range = shown.first().zip(shown.last());
        let label = range
            .and_then(|(&a, &b)| {
                spec.holes
                    .iter()
                    .find(|h| h.lines.0 <= b && a <= h.lines.1)
                    .map(|h| h.label.clone())
            })
            .unwrap_or_default();
        let indent = hidden
            .iter()
            .find(|r| !r.text.trim().is_empty())
            .map(|r| r.text.len() - r.text.trim_start().len())
            .unwrap_or(0);
        out.push(Row {
            kind: RowKind::Hole {
                label,
                count,
                changed,
            },
            old: None,
            new: range.map(|(a, _)| *a),
            text: " ".repeat(indent),
            spans: Vec::new(),
            focus: false,
            note: None,
        });
        hidden.clear();
    };
    for r in picked {
        if r.kind == Line::Del && !diff {
            if !hidden.is_empty() {
                hidden.push(r);
            }
            continue;
        }
        if !keep(r) {
            hidden.push(r);
            continue;
        }
        flush(&mut hidden, &mut out);
        out.push(row(r));
    }
    flush(&mut hidden, &mut out);
    out
}

fn build_file(f: &FileData) -> FileView {
    let rows = unified(f);
    let (head_hl, base_hl, head, base) = match f.lang {
        Some(l) => (
            f.head.as_deref().map(|s| highlight::highlight(l, s)).unwrap_or_default(),
            f.base.as_deref().map(|s| highlight::highlight(l, s)).unwrap_or_default(),
            f.head.as_deref().map(|s| syntax::outline(l, s)).unwrap_or_default(),
            f.base.as_deref().map(|s| syntax::outline(l, s)).unwrap_or_default(),
        ),
        None => Default::default(),
    };
    let tree = f.lang.zip(f.head.as_deref()).and_then(|(l, s)| syntax::parse(l, s));
    FileView {
        rows,
        tree,
        head_hl,
        base_hl,
        head,
        base,
    }
}

fn unified(f: &FileData) -> Vec<URow> {
    let head: Vec<&str> = f.head.as_deref().map(|s| s.lines().collect()).unwrap_or_default();
    let mut out = Vec::new();
    let (mut old, mut new) = (1usize, 1usize);
    let ctx = |old: &mut usize, new: &mut usize, out: &mut Vec<URow>| {
        out.push(URow {
            kind: Line::Ctx,
            old: Some(*old),
            new: Some(*new),
            text: head.get(*new - 1).unwrap_or(&"").to_string(),
            hunk: None,
        });
        *old += 1;
        *new += 1;
    };
    for (hi, h) in f.diff.hunks.iter().enumerate() {
        let target = if h.old_len == 0 { h.old_start + 1 } else { h.old_start };
        while old < target && new <= head.len() {
            ctx(&mut old, &mut new, &mut out);
        }
        for l in &h.lines {
            let (kind, rest) = match l.as_bytes().first() {
                Some(b'+') => (Line::Add, &l[1..]),
                Some(b'-') => (Line::Del, &l[1..]),
                Some(b' ') => (Line::Ctx, &l[1..]),
                _ => continue,
            };
            out.push(URow {
                kind,
                old: (kind != Line::Add).then_some(old),
                new: (kind != Line::Del).then_some(new),
                text: rest.to_string(),
                hunk: Some(hi),
            });
            if kind != Line::Add {
                old += 1;
            }
            if kind != Line::Del {
                new += 1;
            }
        }
    }
    while new <= head.len() {
        ctx(&mut old, &mut new, &mut out);
    }
    out
}

pub fn kind_label(k: DeclKind) -> &'static str {
    match k {
        DeclKind::Type => "type",
        DeclKind::TypeAlias => "alias",
        DeclKind::Class => "class",
        DeclKind::Instance => "instance",
        DeclKind::Function => "fn",
        DeclKind::Port => "port",
        DeclKind::Other => "decl",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{FileDiff, FileStatus, Hunk};

    fn data(base: &str, head: &str, hunks: Vec<Hunk>) -> FileData {
        FileData {
            diff: FileDiff {
                path: "A.hs".into(),
                old_path: None,
                status: FileStatus::Modified,
                binary: false,
                hunks,
            },
            lang: Some(syntax::Lang::Haskell),
            base: Some(base.into()),
            head: Some(head.into()),
        }
    }

    #[test]
    fn unified_puts_removed_lines_in_place() {
        let f = data(
            "a\nb\nc\nd\n",
            "a\nB\nc\nd\ne\n",
            vec![
                Hunk {
                    id: "H1".into(),
                    old_start: 2,
                    old_len: 1,
                    new_start: 2,
                    new_len: 1,
                    lines: vec!["-b".into(), "+B".into()],
                },
                Hunk {
                    id: "H2".into(),
                    old_start: 4,
                    old_len: 0,
                    new_start: 5,
                    new_len: 1,
                    lines: vec!["+e".into()],
                },
            ],
        );
        let rows = unified(&f);
        let texts: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, vec!["a", "b", "B", "c", "d", "e"]);
        let at = |i: usize| (rows[i].kind, rows[i].old, rows[i].new);
        assert_eq!(at(1), (Line::Del, Some(2), None));
        assert_eq!(at(2), (Line::Add, None, Some(2)));
        assert_eq!(at(5), (Line::Add, None, Some(5)));
    }
}
