use crate::lens::{LensSpec, Resolution};
use crate::manifest::{Change, Place, Workspace};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tour {
    pub title: String,
    #[serde(default)]
    pub intent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    #[serde(default)]
    pub glossary: Vec<Gloss>,
    pub stages: Vec<Stage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gloss {
    pub name: String,
    #[serde(default)]
    pub meaning: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stage {
    pub title: String,
    #[serde(default)]
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub title: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Block {
    Text(String),
    Lens(LensSpec),
    Map,
    Glossary,
    Pattern(String),
    Mechanical,
}

const SKELETON_MIN_FOLD: usize = 4;
const SKELETON_MIN_ANCHORS: usize = 4;

pub fn auto(ws: &Workspace) -> Tour {
    let m = &ws.manifest;
    let core: Vec<&crate::manifest::Symbol> = m
        .order
        .iter()
        .filter_map(|id| ws.symbol(id))
        .filter(|s| !s.mechanical)
        .collect();
    let mut stages = Vec::new();

    let mech_hunks = m.hunks.iter().filter(|h| h.mechanical.is_some()).count();
    stages.push(Stage {
        title: "At a glance".into(),
        steps: vec![Step {
            title: "The change map".into(),
            text: format!(
                "{} files, {} changed symbols, {} of {} hunks mechanical.",
                m.files.len(),
                core.len(),
                mech_hunks,
                m.hunks.len()
            ),
            blocks: vec![Block::Map],
        }],
    });

    if !core.is_empty() {
        stages.push(Stage {
            title: "Vocabulary".into(),
            steps: vec![Step {
                title: "New and changed names".into(),
                text: "The names this change adds or changes, in the order the tour uses them.".into(),
                blocks: vec![Block::Glossary],
            }],
        });
    }

    let mut by_file: Vec<(&str, Vec<&crate::manifest::Symbol>)> = Vec::new();
    for s in &core {
        match by_file.iter_mut().find(|(f, _)| *f == s.file) {
            Some((_, v)) => v.push(s),
            None => by_file.push((&s.file, vec![s])),
        }
    }
    let shapes: Vec<Step> = by_file
        .iter()
        .map(|(file, syms)| Step {
            title: short(file),
            text: String::new(),
            blocks: syms
                .iter()
                .map(|s| {
                    let res = if s.change == Change::Removed || (s.kind.is_type_level() && s.change == Change::Added) {
                        Resolution::Full
                    } else {
                        Resolution::Signature
                    };
                    Block::Lens(LensSpec::symbol(&s.id, res))
                })
                .collect(),
        })
        .collect();
    if !shapes.is_empty() {
        stages.push(Stage {
            title: "Shapes".into(),
            steps: shapes,
        });
    }

    let mut parsed = HashMap::new();
    let logic: Vec<Step> = core
        .iter()
        .filter(|s| {
            !s.kind.is_type_level()
                && s.change != Change::Removed
                && skeleton_folds(ws, &mut parsed, s) >= SKELETON_MIN_FOLD
        })
        .map(|s| Step {
            title: s.name.clone(),
            text: String::new(),
            blocks: vec![Block::Lens(LensSpec::symbol(&s.id, Resolution::Skeleton))],
        })
        .collect();
    if !logic.is_empty() {
        stages.push(Stage {
            title: "Logic".into(),
            steps: logic,
        });
    }

    let details: Vec<Step> = core
        .iter()
        .map(|s| Step {
            title: s.name.clone(),
            text: String::new(),
            blocks: vec![Block::Lens(LensSpec::symbol(&s.id, Resolution::Full))],
        })
        .collect();
    if !details.is_empty() {
        stages.push(Stage {
            title: "Details".into(),
            steps: details,
        });
    }

    let mut mech: Vec<Step> = m
        .patterns
        .iter()
        .map(|p| {
            let mut blocks = vec![Block::Pattern(p.id.clone())];
            blocks.extend(p.hunks.first().map(|h| Block::Lens(LensSpec::hunk(h))));
            blocks.extend(p.outliers.iter().map(|o| Block::Lens(LensSpec::hunk(&o.hunk))));
            Step {
                title: format!("Pattern {}", p.id),
                text: String::new(),
                blocks,
            }
        })
        .collect();
    if m.hunks.iter().any(|h| {
        matches!(
            h.mechanical,
            Some(crate::mechanical::Mechanical::Formatting)
                | Some(crate::mechanical::Mechanical::Imports)
                | Some(crate::mechanical::Mechanical::Generated)
        )
    }) {
        mech.push(Step {
            title: "Formatting, imports, generated".into(),
            text: String::new(),
            blocks: vec![Block::Mechanical],
        });
    }
    if !mech.is_empty() {
        stages.push(Stage {
            title: "Mechanical".into(),
            steps: mech,
        });
    }

    let mut loose: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for h in &m.hunks {
        if h.place != Place::Symbols && h.mechanical.is_none() {
            loose.entry(&h.file).or_default().push(&h.id);
        }
    }
    let rest: Vec<Step> = loose
        .into_iter()
        .map(|(file, hs)| Step {
            title: short(file),
            text: String::new(),
            blocks: hs.into_iter().map(|h| Block::Lens(LensSpec::hunk(h))).collect(),
        })
        .collect();
    if !rest.is_empty() {
        stages.push(Stage {
            title: "Everything else".into(),
            steps: rest,
        });
    }

    Tour {
        title: format!("{}..{}", m.base, m.head),
        intent: String::new(),
        range: None,
        glossary: Vec::new(),
        stages,
    }
}

/// Lines that a skeleton of `s` hides, or 0 when it shows too little
/// structure to help.
fn skeleton_folds(
    ws: &Workspace,
    parsed: &mut HashMap<String, Option<(tree_sitter::Tree, crate::syntax::Outline)>>,
    s: &crate::manifest::Symbol,
) -> usize {
    let Some(f) = ws.file(&s.file) else { return 0 };
    let (Some(lang), Some(src)) = (f.lang, f.head.as_deref()) else {
        return 0;
    };
    let Some((tree, outline)) = parsed
        .entry(s.file.clone())
        .or_insert_with(|| Some((crate::syntax::parse(lang, src)?, crate::syntax::outline(lang, src))))
    else {
        return 0;
    };
    let (a, b) = s.lines;
    let mut kept = crate::syntax::anchors(lang, tree, a, b);
    if kept.len() < SKELETON_MIN_ANCHORS {
        return 0;
    }
    if let Some((sa, sb)) = outline.decls.iter().find(|d| d.start == a).and_then(|d| d.sig_lines) {
        kept.extend(sa..=sb);
    }
    (b - a + 1).saturating_sub(kept.len())
}

pub fn short(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}
