use crate::diff::Hunk;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "pattern")]
pub enum Mechanical {
    Generated,
    Formatting,
    Imports,
    Pattern(String),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Substitution {
    pub old: String,
    pub new: String,
}

#[derive(Debug, Serialize)]
pub struct Outlier {
    pub hunk: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct Pattern {
    pub id: String,
    pub substitutions: Vec<Substitution>,
    pub hunks: Vec<String>,
    pub sites: usize,
    pub files: usize,
    pub outliers: Vec<Outlier>,
    /// `file:line` places in head that still use an old name.
    pub leftovers: Vec<String>,
}

pub struct Input<'a> {
    pub file: &'a str,
    pub hunk: &'a Hunk,
    pub generated: bool,
    pub in_imports: bool,
}

pub struct Analysis {
    pub kinds: BTreeMap<String, Mechanical>,
    pub patterns: Vec<Pattern>,
}

const MIN_HUNKS: usize = 2;

type Subs = Option<Vec<(Substitution, usize)>>;

pub fn analyse(inputs: &[Input]) -> Analysis {
    let mut kinds: BTreeMap<String, Mechanical> = BTreeMap::new();
    let mut subs: Vec<(usize, Subs)> = Vec::new();
    for (i, inp) in inputs.iter().enumerate() {
        let id = inp.hunk.id.clone();
        if inp.generated {
            kinds.insert(id, Mechanical::Generated);
        } else if is_formatting(inp.hunk) {
            kinds.insert(id, Mechanical::Formatting);
        } else {
            subs.push((i, substitutions(inp.hunk)));
        }
    }

    let mut atom_hunks: BTreeMap<&Substitution, BTreeSet<usize>> = BTreeMap::new();
    for (i, s) in &subs {
        for (sub, _) in s.iter().flatten().filter(|(x, _)| x.is_rewrite()) {
            atom_hunks.entry(sub).or_default().insert(*i);
        }
    }
    let mut frequent: BTreeSet<&Substitution> = atom_hunks
        .iter()
        .filter(|(_, hs)| hs.len() >= MIN_HUNKS)
        .map(|(s, _)| *s)
        .collect();

    let (mut patterns, explained) = loop {
        let (patterns, explained) = assign(inputs, &subs, &frequent, &atom_hunks);
        let weak: Vec<&Substitution> = patterns
            .iter()
            .filter(|p| p.hunks.len() < MIN_HUNKS)
            .flat_map(|p| p.substitutions.iter())
            .filter_map(|s| frequent.get(s).copied())
            .collect();
        if weak.is_empty() {
            break (patterns, explained);
        }
        for w in weak {
            frequent.remove(w);
        }
    };

    for (n, p) in patterns.iter_mut().enumerate() {
        p.id = format!("P{}", n + 1);
    }
    for (i, _) in &subs {
        let inp = &inputs[*i];
        if let Some(&p) = explained.get(i) {
            kinds.insert(inp.hunk.id.clone(), Mechanical::Pattern(patterns[p].id.clone()));
        } else if inp.in_imports {
            kinds.insert(inp.hunk.id.clone(), Mechanical::Imports);
        }
    }
    Analysis { kinds, patterns }
}

/// Patterns over the `frequent` substitutions, and for each hunk that they
/// fully explain, the index of its first pattern.
fn assign(
    inputs: &[Input],
    subs: &[(usize, Subs)],
    frequent: &BTreeSet<&Substitution>,
    atom_hunks: &BTreeMap<&Substitution, BTreeSet<usize>>,
) -> (Vec<Pattern>, BTreeMap<usize, usize>) {
    let mut groups: BTreeMap<&BTreeSet<usize>, Vec<&Substitution>> = BTreeMap::new();
    for s in frequent {
        groups.entry(&atom_hunks[*s]).or_default().push(s);
    }
    let mut groups: Vec<(&BTreeSet<usize>, Vec<&Substitution>)> = groups.into_iter().collect();
    groups.sort_by_key(|(hs, subs)| (std::cmp::Reverse(hs.len()), subs[0].old.clone()));

    let mut pattern_of: BTreeMap<&Substitution, usize> = BTreeMap::new();
    let mut patterns: Vec<Pattern> = Vec::new();
    for (p, (_, group)) in groups.iter().enumerate() {
        for s in group {
            pattern_of.insert(s, p);
        }
        patterns.push(Pattern {
            id: String::new(),
            substitutions: group.iter().map(|s| (*s).clone()).collect(),
            hunks: Vec::new(),
            sites: 0,
            files: 0,
            outliers: Vec::new(),
            leftovers: Vec::new(),
        });
    }
    let olds: BTreeMap<&str, usize> = pattern_of
        .iter()
        .filter(|(s, _)| is_identifier(&s.old))
        .map(|(s, p)| (s.old.as_str(), *p))
        .collect();

    let mut explained: BTreeMap<usize, usize> = BTreeMap::new();
    let mut files: Vec<BTreeSet<&str>> = vec![BTreeSet::new(); patterns.len()];
    for (i, s) in subs {
        let Some(s) = s else { continue };
        let id = &inputs[*i].hunk.id;
        let hit: BTreeSet<usize> = s.iter().filter_map(|(x, _)| pattern_of.get(x).copied()).collect();
        let stray: Vec<&Substitution> = s
            .iter()
            .map(|(x, _)| x)
            .filter(|x| !pattern_of.contains_key(x))
            .collect();
        if !hit.is_empty() && stray.is_empty() {
            for (x, n) in s {
                let p = pattern_of[x];
                patterns[p].sites += n;
                files[p].insert(inputs[*i].file);
            }
            for &p in &hit {
                patterns[p].hunks.push(id.clone());
            }
            explained.insert(*i, *hit.iter().next().unwrap());
            continue;
        }
        let mut flagged: BTreeSet<usize> = BTreeSet::new();
        for x in &stray {
            if let Some(&p) = olds.get(x.old.as_str())
                && flagged.insert(p)
            {
                patterns[p].outliers.push(Outlier {
                    hunk: id.clone(),
                    reason: format!("`{}` becomes `{}` here", x.old, x.new),
                });
            }
        }
        for p in hit {
            if flagged.insert(p) {
                patterns[p].outliers.push(Outlier {
                    hunk: id.clone(),
                    reason: "has other changes too".into(),
                });
            }
        }
    }
    for (p, f) in patterns.iter_mut().zip(files) {
        p.files = f.len();
    }
    (patterns, explained)
}

impl Substitution {
    /// Both sides carry a word. Pure insertions, pure deletions, and
    /// punctuation edits do not repeat in a way a reader can skim.
    fn is_rewrite(&self) -> bool {
        let wordy = |s: &str| s.chars().any(char::is_alphanumeric);
        wordy(&self.old) && wordy(&self.new)
    }
}

pub fn is_identifier(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && cs.all(|c| c.is_alphanumeric() || c == '_' || c == '\'')
}

fn is_formatting(h: &Hunk) -> bool {
    let squash = |sign: char| -> String {
        h.lines
            .iter()
            .filter_map(|l| l.strip_prefix(sign))
            .flat_map(|l| l.chars().filter(|c| !c.is_whitespace()))
            .collect()
    };
    let changed = h.lines.iter().any(|l| l.starts_with('+') || l.starts_with('-'));
    changed && squash('-') == squash('+')
}

/// The token substitutions of a hunk, with their counts, or `None` when
/// a block of the hunk does not pair removed lines one to one with added
/// lines.
fn substitutions(h: &Hunk) -> Option<Vec<(Substitution, usize)>> {
    let mut counts: BTreeMap<Substitution, usize> = BTreeMap::new();
    let (mut olds, mut news): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
    let mut flush = |olds: &mut Vec<&str>, news: &mut Vec<&str>| -> bool {
        if olds.len() != news.len() {
            return false;
        }
        for (o, n) in olds.iter().zip(news.iter()) {
            for s in line_substitutions(o, n) {
                *counts.entry(s).or_default() += 1;
            }
        }
        olds.clear();
        news.clear();
        true
    };
    for l in &h.lines {
        if let Some(o) = l.strip_prefix('-') {
            if !news.is_empty() && !flush(&mut olds, &mut news) {
                return None;
            }
            olds.push(o);
        } else if let Some(n) = l.strip_prefix('+') {
            news.push(n);
        } else if !flush(&mut olds, &mut news) {
            return None;
        }
    }
    if !flush(&mut olds, &mut news) || counts.is_empty() {
        return None;
    }
    Some(counts.into_iter().collect())
}

fn tokens(line: &str) -> Vec<(usize, usize)> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = line[i..].chars().next().unwrap();
        let len = c.len_utf8();
        if c.is_whitespace() {
            i += len;
        } else if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < b.len() {
                let c = line[i..].chars().next().unwrap();
                if c.is_alphanumeric() || c == '_' || c == '\'' {
                    i += c.len_utf8();
                } else {
                    break;
                }
            }
            out.push((start, i));
        } else {
            out.push((i, i + len));
            i += len;
        }
    }
    out
}

const MAX_LCS_CELLS: usize = 1 << 20;

fn span(line: &str, ts: &[(usize, usize)]) -> String {
    match (ts.first(), ts.last()) {
        (Some(a), Some(b)) => crate::syntax::normalize(&line[a.0..b.1]),
        _ => String::new(),
    }
}

fn text(line: &str, t: (usize, usize)) -> &str {
    &line[t.0..t.1]
}

fn line_substitutions(old: &str, new: &str) -> Vec<Substitution> {
    let (to, tn) = (tokens(old), tokens(new));
    let same = |a: &(usize, usize), b: &(usize, usize)| text(old, *a) == text(new, *b);
    let prefix = to.iter().zip(&tn).take_while(|(a, b)| same(a, b)).count();
    let suffix = to[prefix..]
        .iter()
        .rev()
        .zip(tn[prefix..].iter().rev())
        .take_while(|(a, b)| same(a, b))
        .count();
    let (to, tn) = (&to[prefix..to.len() - suffix], &tn[prefix..tn.len() - suffix]);
    let (n, m) = (to.len(), tn.len());
    if n.saturating_mul(m) > MAX_LCS_CELLS {
        return vec![Substitution {
            old: span(old, to),
            new: span(new, tn),
        }];
    }
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if text(old, to[i]) == text(new, tn[j]) {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    let (mut ro, mut rn) = (i, j);
    loop {
        let matched = i < n && j < m && text(old, to[i]) == text(new, tn[j]);
        if matched || (i == n && j == m) {
            if ro < i || rn < j {
                out.push(Substitution {
                    old: span(old, &to[ro..i]),
                    new: span(new, &tn[rn..j]),
                });
            }
            if i == n && j == m {
                break;
            }
            i += 1;
            j += 1;
            (ro, rn) = (i, j);
        } else if j == m || (i < n && lcs[i + 1][j] >= lcs[i][j + 1]) {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(id: &str, lines: &[&str]) -> Hunk {
        Hunk {
            id: id.into(),
            old_start: 1,
            old_len: 0,
            new_start: 1,
            new_len: 0,
            lines: lines.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn sub(old: &str, new: &str) -> Substitution {
        Substitution {
            old: old.into(),
            new: new.into(),
        }
    }

    #[test]
    fn line_substitutions_find_replaced_runs() {
        assert_eq!(
            line_substitutions("  if isRequired p then a", "  if approvalPolicy p then a"),
            vec![sub("isRequired", "approvalPolicy")]
        );
        assert_eq!(
            line_substitutions("f (Foo.bar x)", "f (Baz.bar x) y"),
            vec![sub("Foo", "Baz"), sub("", "y")]
        );
    }

    #[test]
    fn huge_lines_skip_the_lcs_table() {
        let old = format!("a {} z", "x ".repeat(5000));
        let new = format!("a {} z", "y ".repeat(5000));
        let subs = line_substitutions(&old, &new);
        assert_eq!(subs.len(), 1);
        assert!(subs[0].old.starts_with("x x") && subs[0].new.starts_with("y y"));
    }

    #[test]
    fn formatting_ignores_whitespace_and_line_breaks() {
        assert!(is_formatting(&hunk("H1", &["-f x  =  y", "+f x =", "+  y"])));
        assert!(!is_formatting(&hunk("H1", &["-f x = y", "+f x = z"])));
    }

    #[test]
    fn unpaired_blocks_are_not_substitutions() {
        assert_eq!(substitutions(&hunk("H1", &["-a", "+b", "+c"])), None);
        assert_eq!(
            substitutions(&hunk("H1", &[" ctx", "-a x", "+b x", " ctx", "-a y", "+b y"])),
            Some(vec![(sub("a", "b"), 2)])
        );
    }

    #[test]
    fn patterns_and_outliers() {
        let hs = [
            hunk("H1", &["-  isRequired p", "+  approvalPolicy p"]),
            hunk("H2", &["-  x = isRequired q", "+  x = approvalPolicy q"]),
            hunk("H3", &["-  isRequired r", "+  isMandatory r"]),
            hunk("H4", &["-  isRequired s 1", "+  approvalPolicy s 2"]),
            hunk("H5", &["-f  x", "+f x"]),
            hunk("H6", &["-import A", "+import B", "+import C"]),
            hunk("H7", &["-x", "+y", "+z"]),
        ];
        let inputs: Vec<Input> = hs
            .iter()
            .map(|h| Input {
                file: "A.hs",
                hunk: h,
                generated: false,
                in_imports: h.id == "H6",
            })
            .collect();
        let a = analyse(&inputs);
        assert_eq!(a.patterns.len(), 1);
        let p = &a.patterns[0];
        assert_eq!(p.substitutions, vec![sub("isRequired", "approvalPolicy")]);
        assert_eq!(p.hunks, vec!["H1", "H2"]);
        assert_eq!(p.sites, 2);
        let outliers: Vec<&str> = p.outliers.iter().map(|o| o.hunk.as_str()).collect();
        assert_eq!(outliers, vec!["H3", "H4"]);
        assert_eq!(a.kinds.get("H1"), Some(&Mechanical::Pattern("P1".into())));
        assert_eq!(a.kinds.get("H3"), None);
        assert_eq!(a.kinds.get("H5"), Some(&Mechanical::Formatting));
        assert_eq!(a.kinds.get("H6"), Some(&Mechanical::Imports));
        assert_eq!(a.kinds.get("H7"), None);
    }

    #[test]
    fn insertions_punctuation_and_weak_patterns_are_not_patterns() {
        let hs = [
            hunk("H1", &["-f a", "+f a b"]),
            hunk("H2", &["-g a", "+g a b"]),
            hunk("H3", &["-x)", "+x"]),
            hunk("H4", &["-y)", "+y"]),
            hunk("H5", &["-p old", "+p new"]),
            hunk("H6", &["-q old z", "+q new w"]),
        ];
        let inputs: Vec<Input> = hs
            .iter()
            .map(|h| Input {
                file: "A.hs",
                hunk: h,
                generated: false,
                in_imports: false,
            })
            .collect();
        let a = analyse(&inputs);
        assert!(a.patterns.is_empty(), "{:?}", a.patterns);
        assert!(a.kinds.is_empty());
    }
}
