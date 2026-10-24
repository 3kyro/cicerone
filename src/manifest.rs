use crate::diff::{FileDiff, FileStatus};
use crate::git::{Range, Repo};
use crate::syntax::{self, Decl, DeclKind, Lang, Outline, Region};
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Added,
    Removed,
    SignatureChanged,
    BodyChanged,
    DocChanged,
    Formatting,
}

#[derive(Debug, Serialize)]
pub struct Symbol {
    pub id: String,
    pub file: String,
    pub name: String,
    pub kind: DeclKind,
    pub change: Change,
    pub signature: Option<String>,
    pub old_signature: Option<String>,
    /// Head lines, or base lines for a removed symbol.
    pub lines: (usize, usize),
    pub hunks: Vec<String>,
    pub uses: Vec<String>,
    pub used_by: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Place {
    Symbols,
    Header,
    Imports,
    Other,
}

#[derive(Debug, Serialize)]
pub struct HunkEntry {
    pub id: String,
    pub file: String,
    pub old: (usize, usize),
    pub new: (usize, usize),
    pub added: usize,
    pub removed: usize,
    pub place: Place,
    pub symbols: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub lang: Option<Lang>,
    pub binary: bool,
    pub added: usize,
    pub removed: usize,
    pub hunks: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Manifest {
    pub base: String,
    pub head: String,
    pub files: Vec<FileEntry>,
    pub symbols: Vec<Symbol>,
    pub hunks: Vec<HunkEntry>,
    /// Symbol ids, each one after the changed symbols that it uses.
    pub order: Vec<String>,
}

pub fn build(repo: &Repo, range: &Range) -> Result<Manifest> {
    let diff = crate::diff::parse(&repo.diff(range)?);
    let mut m = Manifest {
        base: range.base.clone(),
        head: range.head.clone().unwrap_or_else(|| "worktree".into()),
        files: Vec::new(),
        symbols: Vec::new(),
        hunks: Vec::new(),
        order: Vec::new(),
    };
    let mut refs: Vec<Scope> = Vec::new();
    for file in &diff {
        let lang = Lang::of_path(&file.path);
        let (base, head) = match lang {
            Some(l) if !file.binary => {
                let load = |rev: Option<&str>, path: &str, absent: bool| {
                    if absent {
                        Outline::default()
                    } else {
                        repo.show(rev, path)
                            .map(|s| syntax::outline(l, &s))
                            .unwrap_or_default()
                    }
                };
                (
                    load(Some(&range.base), file.base_path(), file.status == FileStatus::Added),
                    load(range.head.as_deref(), &file.path, file.status == FileStatus::Deleted),
                )
            }
            _ => (Outline::default(), Outline::default()),
        };
        add_file(&mut m, &mut refs, file, lang, &base, &head);
    }
    link(&mut m, &refs);
    m.order = order(&m);
    Ok(m)
}

/// What a symbol refers to, and what its file can see.
struct Scope {
    refs: BTreeSet<String>,
    module: Option<String>,
    imports: BTreeSet<String>,
}

fn add_file(
    m: &mut Manifest,
    refs: &mut Vec<Scope>,
    file: &FileDiff,
    lang: Option<Lang>,
    base: &Outline,
    head: &Outline,
) {
    let changed: Vec<(usize, usize)> = file.hunks.iter().map(|h| h.changed_lines()).fold(
        Vec::new(),
        |mut acc, (o, n)| {
            acc.extend(o.into_iter().map(|l| (0, l)));
            acc.extend(n.into_iter().map(|l| (1, l)));
            acc
        },
    );
    let touched = |side: usize, d: &Decl| {
        changed
            .iter()
            .any(|&(s, l)| s == side && d.start <= l && l <= d.end)
    };
    let key = |d: &Decl| (d.kind, d.name.clone());
    let base_by_key: BTreeMap<_, &Decl> = base.decls.iter().map(|d| (key(d), d)).collect();
    let head_keys: BTreeSet<_> = head.decls.iter().map(key).collect();

    let mut head_ids: BTreeMap<usize, String> = BTreeMap::new();
    let mut base_ids: BTreeMap<usize, String> = BTreeMap::new();
    let mut push = |m: &mut Manifest, d: &Decl, change, old: Option<&Decl>| {
        let o = if change == Change::Removed { base } else { head };
        let id = format!("S{}", m.symbols.len() + 1);
        m.symbols.push(Symbol {
            id: id.clone(),
            file: file.path.clone(),
            name: d.name.clone(),
            kind: d.kind,
            change,
            signature: d.signature.clone(),
            old_signature: old.and_then(|o| o.signature.clone()),
            lines: (d.start, d.end),
            hunks: Vec::new(),
            uses: Vec::new(),
            used_by: Vec::new(),
        });
        refs.push(Scope {
            refs: d.refs.clone(),
            module: o.module.clone(),
            imports: o.imports.clone(),
        });
        id
    };

    for (i, d) in head.decls.iter().enumerate() {
        let change = match base_by_key.get(&key(d)) {
            None => Some(Change::Added),
            Some(b) if b.text == d.text => touched(1, d).then_some(Change::DocChanged),
            Some(b) if syntax::normalize(&b.text) == syntax::normalize(&d.text) => {
                Some(Change::Formatting)
            }
            Some(b) => match (&b.signature, &d.signature) {
                (Some(x), Some(y)) if syntax::normalize(x) != syntax::normalize(y) => {
                    Some(Change::SignatureChanged)
                }
                _ => Some(Change::BodyChanged),
            },
        };
        if let Some(c) = change {
            let old = base_by_key.get(&key(d)).copied();
            let id = push(m, d, c, old.filter(|_| c == Change::SignatureChanged));
            head_ids.insert(i, id.clone());
            if let Some(bi) = old.and_then(|o| base.decls.iter().position(|x| std::ptr::eq(x, o))) {
                base_ids.insert(bi, id);
            }
        }
    }
    for (i, d) in base.decls.iter().enumerate() {
        if !head_keys.contains(&key(d)) {
            let id = push(m, d, Change::Removed, None);
            base_ids.insert(i, id);
        }
    }

    let mut entry = FileEntry {
        path: file.path.clone(),
        old_path: file.old_path.clone(),
        status: file.status,
        lang,
        binary: file.binary,
        added: 0,
        removed: 0,
        hunks: Vec::new(),
    };
    for h in &file.hunks {
        let (olds, news) = h.changed_lines();
        let mut syms: BTreeSet<String> = BTreeSet::new();
        for &l in &news {
            syms.extend(head.decls_at(l).filter_map(|i| head_ids.get(&i).cloned()));
        }
        for &l in &olds {
            syms.extend(base.decls_at(l).filter_map(|i| base_ids.get(&i).cloned()));
        }
        let place = if !syms.is_empty() {
            Place::Symbols
        } else {
            let region = news
                .iter()
                .find_map(|&l| head.region_at(l))
                .or_else(|| olds.iter().find_map(|&l| base.region_at(l)));
            match region {
                Some(Region::Header) => Place::Header,
                Some(Region::Imports) => Place::Imports,
                None => Place::Other,
            }
        };
        let mut syms: Vec<String> = syms.into_iter().collect();
        syms.sort_by_key(|s| s[1..].parse::<usize>().unwrap_or(0));
        for s in &syms {
            if let Some(sym) = m.symbols.iter_mut().find(|x| &x.id == s) {
                sym.hunks.push(h.id.clone());
            }
        }
        entry.added += h.added();
        entry.removed += h.removed();
        entry.hunks.push(h.id.clone());
        m.hunks.push(HunkEntry {
            id: h.id.clone(),
            file: file.path.clone(),
            old: (h.old_start, h.old_len),
            new: (h.new_start, h.new_len),
            added: h.added(),
            removed: h.removed(),
            place,
            symbols: syms,
        });
    }
    m.files.push(entry);
}

fn link(m: &mut Manifest, scopes: &[Scope]) {
    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, s) in m.symbols.iter().enumerate() {
        if s.change != Change::Removed && s.kind != DeclKind::Instance {
            by_name.entry(s.name.as_str()).or_default().push(i);
        }
    }
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for (i, sc) in scopes.iter().enumerate() {
        for name in &sc.refs {
            for &j in by_name.get(name.as_str()).into_iter().flatten() {
                let visible = m.symbols[i].file == m.symbols[j].file
                    || scopes[j]
                        .module
                        .as_ref()
                        .is_some_and(|mj| sc.imports.contains(mj));
                if i != j && visible {
                    edges.push((i, j));
                }
            }
        }
    }
    for (i, j) in edges {
        let (ui, uj) = (m.symbols[i].id.clone(), m.symbols[j].id.clone());
        m.symbols[i].uses.push(uj);
        m.symbols[j].used_by.push(ui);
    }
}

fn order(m: &Manifest) -> Vec<String> {
    let idx: BTreeMap<&str, usize> = m
        .symbols
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();
    let mut roots: Vec<usize> = (0..m.symbols.len()).collect();
    roots.sort_by_key(|&i| {
        let s = &m.symbols[i];
        (!s.kind.is_type_level(), s.change == Change::Removed, std::cmp::Reverse(s.used_by.len()))
    });
    let mut seen = vec![false; m.symbols.len()];
    let mut out = Vec::new();
    fn visit(
        i: usize,
        m: &Manifest,
        idx: &BTreeMap<&str, usize>,
        seen: &mut [bool],
        out: &mut Vec<String>,
    ) {
        if seen[i] {
            return;
        }
        seen[i] = true;
        for u in &m.symbols[i].uses {
            visit(idx[u.as_str()], m, idx, seen, out);
        }
        out.push(m.symbols[i].id.clone());
    }
    for r in roots {
        visit(r, m, &idx, &mut seen, &mut out);
    }
    out
}
