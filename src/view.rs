use crate::lens::{LensView, RowKind, kind_label};
use crate::manifest::{Change, Symbol, Workspace};
use crate::mechanical::Mechanical;
use crate::review::{Finding, Severity};
use crate::syntax::{self, DeclKind};
use crate::theme::{self, *};
use crate::tour::{Tour, short};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

type Lines = Vec<Line<'static>>;

fn width(spans: &[Span]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// Paragraphs with `code` spans, **bold**, and `- ` bullets, wrapped to `w`.
pub fn markdown(src: &str, w: usize) -> Lines {
    let mut out = Vec::new();
    for para in src.split("\n\n") {
        for raw in para.lines() {
            let (bullet, body) = match raw.trim_start().strip_prefix("- ") {
                Some(b) => (true, b),
                None => (false, raw.trim()),
            };
            let indent = if bullet { 2 } else { 0 };
            let mut words: Vec<Span<'static>> = Vec::new();
            let mut bold = false;
            for (i, seg) in body.split('`').enumerate() {
                if i % 2 == 1 {
                    words.push(Span::styled(
                        seg.to_string(),
                        Style::default().fg(PEACH).bg(SURFACE),
                    ));
                    continue;
                }
                for (j, part) in seg.split("**").enumerate() {
                    if j > 0 {
                        bold = !bold;
                    }
                    let style = if bold {
                        text().add_modifier(Modifier::BOLD)
                    } else {
                        text()
                    };
                    for (k, word) in part.split(' ').enumerate() {
                        if k > 0 {
                            words.push(Span::styled(" ", style));
                        }
                        if !word.is_empty() {
                            words.push(Span::styled(word.to_string(), style));
                        }
                    }
                }
            }
            let mut line: Vec<Span<'static>> = vec![Span::raw(if bullet { "• " } else { "" })];
            if bullet {
                line[0] = Span::styled("• ", Style::default().fg(MAUVE));
            }
            for word in words {
                if word.content == " " && width(&line) <= indent {
                    continue;
                }
                if width(&line) + word.width() > w && width(&line) > indent {
                    while line.last().is_some_and(|s| s.content == " ") {
                        line.pop();
                    }
                    out.push(Line::from(std::mem::take(&mut line)));
                    line.push(Span::raw(" ".repeat(indent)));
                    if word.content == " " {
                        continue;
                    }
                }
                line.push(word);
            }
            if width(&line) > indent || !bullet {
                out.push(Line::from(line));
            }
        }
        out.push(Line::default());
    }
    while out.last().is_some_and(|l| l.spans.iter().all(|s| s.content.trim().is_empty())) {
        out.pop();
    }
    out
}

fn change_mark(c: Change) -> (&'static str, Style) {
    let s = Style::default();
    match c {
        Change::Added => ("+", s.fg(GREEN)),
        Change::Removed => ("-", s.fg(RED)),
        Change::SignatureChanged => ("±", s.fg(PEACH)),
        Change::BodyChanged => ("~", s.fg(BLUE)),
        Change::DocChanged => ("d", s.fg(OVERLAY)),
        Change::Formatting => ("f", s.fg(OVERLAY)),
    }
}

fn change_word(c: Change) -> &'static str {
    match c {
        Change::Added => "new",
        Change::Removed => "removed",
        Change::SignatureChanged => "signature changed",
        Change::BodyChanged => "body changed",
        Change::DocChanged => "doc changed",
        Change::Formatting => "formatting",
    }
}

fn truncate(spans: Vec<Span<'static>>, w: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut used = 0;
    for s in spans {
        let sw = s.width();
        if used + sw <= w {
            used += sw;
            out.push(s);
            continue;
        }
        let mut cut = String::new();
        for ch in s.content.chars() {
            let cw = unicode_width(ch);
            if used + cw + 1 > w {
                break;
            }
            used += cw;
            cut.push(ch);
        }
        out.push(Span::styled(cut, s.style));
        out.push(Span::styled("…", dim()));
        break;
    }
    out
}

fn unicode_width(c: char) -> usize {
    Span::raw(c.to_string()).width()
}

fn code_spans(text: &str, spans: &[(usize, usize, crate::highlight::Hl)], base: Style) -> Vec<Span<'static>> {
    let text = text.replace('\t', " ");
    let mut out = Vec::new();
    let mut i = 0;
    for &(a, b, h) in spans {
        if a < i || b > text.len() || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
            continue;
        }
        if a > i {
            out.push(Span::styled(text[i..a].to_string(), base.fg(TEXT)));
        }
        out.push(Span::styled(text[a..b].to_string(), base.patch(theme::hl(h))));
        i = b;
    }
    if i < text.len() {
        out.push(Span::styled(text[i..].to_string(), base.fg(TEXT)));
    }
    out
}

/// Drops the first `n` columns of `spans`, for horizontal scroll.
fn skip(spans: Vec<Span<'static>>, mut n: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for s in spans {
        if n == 0 {
            out.push(s);
            continue;
        }
        let mut kept = String::new();
        for ch in s.content.chars() {
            if n > 0 {
                n = n.saturating_sub(unicode_width(ch));
            } else {
                kept.push(ch);
            }
        }
        if !kept.is_empty() {
            out.push(Span::styled(kept, s.style));
        }
    }
    out
}

pub const LENS_GUTTER: usize = 9;
pub const GROUP_GUTTER: usize = 7;

pub fn widest(v: &LensView) -> usize {
    v.rows.iter().map(|r| r.text.chars().count()).max().unwrap_or(0)
}

pub fn group_widest(ws: &Workspace, g: &crate::tour::Group) -> usize {
    g.hunks
        .iter()
        .filter_map(|id| ws.hunk(id))
        .filter_map(|h| ws.file(&h.file)?.diff.hunks.iter().find(|x| x.id == h.id))
        .flat_map(|x| x.lines.iter().filter(|l| l.starts_with('+') || l.starts_with('-')).take(GROUP_LINES))
        .map(|l| l.chars().count().saturating_sub(1))
        .max()
        .unwrap_or(0)
}

pub fn severity_style(s: Severity) -> Style {
    Style::default().fg(match s {
        Severity::Bug => RED,
        Severity::Risk => PEACH,
        Severity::Change => BLUE,
        Severity::Question => YELLOW,
        Severity::Nit => SUBTEXT,
    })
}

/// A finding as a card: its severity and title, then its body, each line
/// after `prefix`.
pub fn finding_card(f: &Finding, prefix: Vec<Span<'static>>, w: usize) -> Lines {
    let pw = width(&prefix);
    let st = if f.withdrawn { dim() } else { severity_style(f.severity) };
    let title = if f.withdrawn {
        dim().add_modifier(Modifier::CROSSED_OUT)
    } else {
        text().add_modifier(Modifier::BOLD)
    };
    let mut head = vec![
        Span::styled("◆ ", st),
        Span::styled(format!("{} ", f.severity.label()), st.add_modifier(Modifier::BOLD)),
    ];
    if f.withdrawn {
        head.push(Span::styled("WITHDRAWN ", Style::default().fg(OVERLAY).add_modifier(Modifier::BOLD)));
    }
    let indent = vec![Span::raw(" ".repeat(width(&head)))];
    let mut out = wrapped([prefix.clone(), head].concat(), [prefix.clone(), indent].concat(), &f.title, title, w);
    if let Some(last) = f.history.last() {
        let (icon, what) = if f.withdrawn {
            ("✕ ", format!("withdrawn: {}", last.reason))
        } else {
            ("↻ ", format!("revised {}×: {}", f.history.len(), last.reason))
        };
        out.push(Line::from(truncate(
            [
                prefix.clone(),
                vec![
                    Span::styled(icon, Style::default().fg(PEACH)),
                    Span::styled(what, Style::default().fg(PEACH).add_modifier(Modifier::ITALIC)),
                ],
            ]
            .concat(),
            w,
        )));
    }
    for l in markdown(&f.body, w.saturating_sub(pw + 2)) {
        let mut spans = prefix.clone();
        spans.push(Span::styled("│ ", st));
        spans.extend(l.spans);
        out.push(Line::from(spans));
    }
    out
}

/// Wraps `src` as markdown after `first` on the first line and `rest` on
/// the others. Plain text takes `style`; code spans keep their own.
pub fn wrapped(first: Vec<Span<'static>>, rest: Vec<Span<'static>>, src: &str, style: Style, w: usize) -> Lines {
    let pw = width(&first).max(width(&rest));
    markdown(src, w.saturating_sub(pw).max(10))
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            let mut spans = if i == 0 { first.clone() } else { rest.clone() };
            spans.extend(l.spans.into_iter().map(|s| {
                if s.style.fg == Some(TEXT) {
                    let st = s.style.patch(style);
                    s.style(st)
                } else {
                    s
                }
            }));
            Line::from(spans)
        })
        .collect()
}

/// `findings` get a mark on their lines; the ones named in `cards` also get
/// their card, under the first line they cover.
/// The earlier states of a finding, newest first, each with why it changed.
pub fn finding_history(f: &Finding, w: usize) -> Lines {
    let mut out = Vec::new();
    if f.history.is_empty() {
        return out;
    }
    out.push(Line::default());
    out.push(Line::from(Span::styled("History", Style::default().fg(OVERLAY).add_modifier(Modifier::BOLD))));
    for (i, r) in f.history.iter().enumerate().rev() {
        let next_body = f.history.get(i + 1).map_or(&f.body, |n| &n.body);
        out.push(Line::from(truncate(
            vec![
                Span::styled(format!("  {}. why it changed: ", i + 1), dim()),
                Span::styled(r.reason.clone(), Style::default().fg(PEACH)),
            ],
            w,
        )));
        let was = if r.withdrawn { "WITHDRAWN " } else { "" };
        out.push(Line::from(truncate(
            vec![
                Span::styled("     before: ", dim()),
                Span::styled(format!("{} {was}", r.severity.label()), severity_style(r.severity)),
                Span::styled(format!("{}  ", r.title), Style::default().fg(SUBTEXT)),
                Span::styled(format!("lines {}-{}", r.lines.0, r.lines.1), dim()),
            ],
            w,
        )));
        if &r.body == next_body {
            continue;
        }
        for l in markdown(&r.body, w.saturating_sub(7)) {
            let mut spans = vec![Span::raw("       ")];
            spans.extend(l.spans.into_iter().map(|s| s.style(dim())));
            out.push(Line::from(spans));
        }
    }
    out
}

pub fn lens(v: &LensView, focused: bool, w: usize, hs: usize, findings: &[&Finding], cards: &[&str]) -> Lines {
    let frame = Style::default().fg(if focused { MAUVE } else { SURFACE });
    let mut out = Vec::new();
    let mut chips: Vec<Span<'static>> = Vec::new();
    for (i, r) in v.available.iter().enumerate() {
        if i > 0 {
            chips.push(Span::styled(" · ", dim()));
        }
        if *r == v.resolution {
            chips.push(Span::styled(
                r.label().to_string(),
                Style::default().fg(if focused { MAUVE } else { LAVENDER }).add_modifier(Modifier::BOLD),
            ));
        } else {
            chips.push(Span::styled(r.label().to_string(), dim()));
        }
    }
    if v.diff && v.available.len() > 1 {
        chips.push(Span::styled("  ±", Style::default().fg(PEACH)));
    }
    let mut head = vec![
        Span::styled("╭─ ", frame),
        Span::styled(v.title.clone(), Style::default().fg(LAVENDER).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(v.place.clone(), dim()),
        Span::raw(" "),
    ];
    let fill = w.saturating_sub(width(&head) + width(&chips) + 2);
    head.push(Span::styled("─".repeat(fill), frame));
    head.push(Span::raw(" "));
    head.extend(chips);
    out.push(Line::from(truncate(head, w)));

    let code_w = w.saturating_sub(LENS_GUTTER);
    let mut carded: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for row in &v.rows {
        let num = row.new.or(row.old).map(|n| format!("{n:>5}")).unwrap_or_else(|| "     ".into());
        let (mark, mark_style, bg) = match (&row.kind, v.diff) {
            (RowKind::Add, true) => ("+", Style::default().fg(GREEN), Some(ADD_BG)),
            (RowKind::Del, _) => ("-", Style::default().fg(RED), Some(DEL_BG)),
            (RowKind::Hole { .. }, _) => ("┆", dim(), None),
            _ => (" ", dim(), None),
        };
        let bg = if row.focus { Some(FOCUS_BG) } else { bg };
        let base = bg.map_or(Style::default(), |b| Style::default().bg(b));
        let hit: Vec<&&Finding> = match (row.new, row.last, &row.kind) {
            (Some(a), Some(b), k) if *k != RowKind::Del => findings.iter().filter(|f| f.hits(&v.file, a, b)).collect(),
            _ => Vec::new(),
        };
        let gutter = match hit.iter().map(|f| f.severity).min() {
            Some(sev) => Span::styled("◆", severity_style(sev)),
            None => Span::styled(if hs > 0 { "«" } else { " " }, Style::default().fg(PEACH)),
        };
        let mut line = vec![
            Span::styled("│", frame),
            Span::styled(num, if row.focus { Style::default().fg(MAUVE) } else { dim() }),
            gutter,
            Span::styled(format!("{mark} "), mark_style.patch(base)),
        ];
        match &row.kind {
            RowKind::Hole { label, count, changed } => {
                let text = if label.is_empty() {
                    format!("⋯ {count} line{}", if *count == 1 { "" } else { "s" })
                } else {
                    format!("⋯ {label}")
                };
                line.push(Span::raw(row.text.get(hs..).unwrap_or("").to_string()));
                line.push(Span::styled(text, dim().add_modifier(Modifier::ITALIC)));
                if *changed {
                    line.push(Span::styled("  ● changes", Style::default().fg(PEACH)));
                }
            }
            _ => {
                let code = truncate(skip(code_spans(&row.text, &row.spans, base), hs), code_w);
                let used = width(&code);
                line.extend(code);
                if bg.is_some() && used < code_w {
                    line.push(Span::styled(" ".repeat(code_w - used), base));
                }
            }
        }
        out.push(Line::from(line));
        for f in hit {
            if cards.contains(&f.id.as_str()) && carded.insert(f.id.as_str()) {
                out.extend(finding_card(f, vec![Span::styled("│", frame), Span::raw("       ")], w));
            }
        }
        if let Some(n) = &row.note {
            out.extend(wrapped(
                vec![Span::styled("│", frame), Span::raw("       "), Span::styled("╰─ ✦ ", Style::default().fg(PEACH))],
                vec![Span::styled("│", frame), Span::raw("            ")],
                n,
                Style::default().fg(PEACH).add_modifier(Modifier::ITALIC),
                w,
            ));
        }
    }
    if v.rows.is_empty() {
        out.push(Line::from(vec![Span::styled("│", frame), Span::styled("  nothing to show", dim())]));
    }
    out.push(Line::from(Span::styled(format!("╰{}", "─".repeat(w.saturating_sub(1).min(12))), frame)));
    out
}

pub fn map(ws: &Workspace, w: usize) -> Lines {
    let m = &ws.manifest;
    let mut out = Vec::new();
    for f in &m.files {
        let (icon, style) = match f.status {
            crate::diff::FileStatus::Added => ("+", Style::default().fg(GREEN)),
            crate::diff::FileStatus::Deleted => ("-", Style::default().fg(RED)),
            crate::diff::FileStatus::Renamed => ("→", Style::default().fg(BLUE)),
            crate::diff::FileStatus::Modified => ("~", Style::default().fg(OVERLAY)),
        };
        let (dir, name) = f.path.rsplit_once('/').unwrap_or(("", &f.path));
        out.push(Line::from(truncate(
            vec![
                Span::styled(format!("{icon} "), style),
                Span::styled(if dir.is_empty() { String::new() } else { format!("{dir}/") }, dim()),
                Span::styled(name.to_string(), text().add_modifier(Modifier::BOLD)),
                Span::styled(format!("  +{}", f.added), Style::default().fg(GREEN)),
                Span::styled(format!(" -{}", f.removed), Style::default().fg(RED)),
            ],
            w,
        )));
        let syms: Vec<&Symbol> = m
            .order
            .iter()
            .filter_map(|id| ws.symbol(id))
            .filter(|s| s.file == f.path && !s.mechanical)
            .collect();
        for s in syms {
            let (mark, ms) = change_mark(s.change);
            out.push(Line::from(truncate(
                vec![
                    Span::raw("    "),
                    Span::styled(format!("{mark} "), ms),
                    Span::styled(
                        format!("{:<8} ", if s.kind == DeclKind::Instance { "" } else { kind_label(s.kind) }),
                        dim(),
                    ),
                    Span::styled(s.name.clone(), Style::default().fg(LAVENDER)),
                ],
                w,
            )));
        }
        let mech = f
            .hunks
            .iter()
            .filter(|h| ws.hunk(h).is_some_and(|x| x.mechanical.is_some()))
            .count();
        if mech > 0 {
            out.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(format!("░ {mech} mechanical hunk{}", if mech == 1 { "" } else { "s" }), dim()),
            ]));
        }
    }
    out
}

pub fn glossary(ws: &Workspace, tour: &Tour, w: usize) -> Lines {
    let mut out = Vec::new();
    let entries: Vec<(String, Option<&Symbol>, String)> = if tour.glossary.is_empty() {
        ws.manifest
            .order
            .iter()
            .filter_map(|id| ws.symbol(id))
            .filter(|s| !s.mechanical)
            .map(|s| (s.name.clone(), Some(s), String::new()))
            .collect()
    } else {
        tour.glossary
            .iter()
            .map(|g| {
                let sym = g
                    .symbol
                    .as_deref()
                    .and_then(|id| ws.symbol(id))
                    .or_else(|| ws.manifest.symbols.iter().find(|s| s.name == g.name));
                (g.name.clone(), sym, g.meaning.clone())
            })
            .collect()
    };
    for (name, sym, meaning) in entries {
        let mut head = vec![Span::styled(name, Style::default().fg(LAVENDER).add_modifier(Modifier::BOLD))];
        if let Some(s) = sym {
            let (mark, ms) = change_mark(s.change);
            if s.kind != DeclKind::Instance {
                head.push(Span::styled(format!("  {} ", kind_label(s.kind)), dim()));
            } else {
                head.push(Span::raw("  "));
            }
            head.push(Span::styled(format!("{mark} {}", change_word(s.change)), ms));
        }
        out.push(Line::from(truncate(head, w)));
        if let Some(sig) = sym.and_then(|s| s.signature.as_deref()) {
            let sig = syntax::normalize(sig);
            let body = sig.split_once("::").or_else(|| sig.split_once(" : ")).map_or(sig.as_str(), |(_, b)| b);
            out.push(Line::from(truncate(
                vec![Span::raw("    "), Span::styled(format!(":: {}", body.trim()), Style::default().fg(TEAL))],
                w,
            )));
        }
        if !meaning.is_empty() {
            for l in markdown(&meaning, w.saturating_sub(4)) {
                let mut spans = vec![Span::raw("    ")];
                spans.extend(l.spans);
                out.push(Line::from(spans));
            }
        }
        out.push(Line::default());
    }
    out.pop();
    out
}

pub fn pattern(ws: &Workspace, id: &str, w: usize) -> Lines {
    let Some(p) = ws.manifest.patterns.iter().find(|p| p.id == id) else {
        return vec![Line::from(Span::styled(format!("no pattern {id}"), dim()))];
    };
    let mut out = Vec::new();
    for s in &p.substitutions {
        out.push(Line::from(truncate(
            vec![
                Span::styled(if s.old.is_empty() { "∅".into() } else { s.old.clone() }, Style::default().fg(RED)),
                Span::styled("  →  ", dim()),
                Span::styled(if s.new.is_empty() { "∅".into() } else { s.new.clone() }, Style::default().fg(GREEN)),
            ],
            w,
        )));
    }
    out.push(Line::from(vec![
        Span::styled(
            format!("{} sites · {} hunks · {} files  ", p.sites, p.hunks.len(), p.files),
            Style::default().fg(SUBTEXT),
        ),
        Span::styled("✓ fully explained", Style::default().fg(GREEN)),
    ]));
    for o in &p.outliers {
        out.push(Line::from(truncate(
            vec![
                Span::styled("⚠ ", Style::default().fg(YELLOW)),
                Span::styled(format!("{} ", o.hunk), Style::default().fg(YELLOW).add_modifier(Modifier::BOLD)),
                Span::styled(o.reason.clone(), text()),
                Span::styled(" — shown below", dim()),
            ],
            w,
        )));
    }
    if !p.leftovers.is_empty() {
        let first: Vec<String> = p.leftovers.iter().take(3).cloned().collect();
        out.extend(wrapped(
            vec![Span::styled("⚠ ", Style::default().fg(YELLOW))],
            vec![Span::raw("  ")],
            &format!("old name still used in {} places: {}", p.leftovers.len(), first.join(", ")),
            text(),
            w,
        ));
    }
    out.push(Line::from(Span::styled(
        "The first hunk below is one example. The others are the same substitution.",
        dim(),
    )));
    out
}

type IsKind = fn(&Mechanical) -> bool;

pub const MECHANICAL_KINDS: [(&str, IsKind); 4] = [
    ("Formatting", |k| *k == Mechanical::Formatting),
    ("Imports", |k| *k == Mechanical::Imports),
    ("Moved code", |k| matches!(k, Mechanical::Moved(_))),
    ("Generated", |k| *k == Mechanical::Generated),
];

pub fn mechanical(ws: &Workspace, w: usize) -> Lines {
    let m = &ws.manifest;
    let mut out = Vec::new();
    for (title, is) in MECHANICAL_KINDS {
        let hs: Vec<_> = m.hunks.iter().filter(|h| h.mechanical.as_ref().is_some_and(is)).collect();
        if hs.is_empty() {
            continue;
        }
        out.push(Line::from(Span::styled(
            format!("{title} · {} hunks", hs.len()),
            Style::default().fg(LAVENDER).add_modifier(Modifier::BOLD),
        )));
        let mut files: Vec<(&str, Vec<String>)> = Vec::new();
        for h in hs {
            let id = match &h.mechanical {
                Some(Mechanical::Moved(o)) if !o.is_empty() => format!("{}↔{}", h.id, o.join(",")),
                _ => h.id.clone(),
            };
            match files.iter_mut().find(|(f, _)| *f == h.file) {
                Some((_, v)) => v.push(id),
                None => files.push((&h.file, vec![id])),
            }
        }
        for (f, ids) in files {
            out.push(Line::from(truncate(
                vec![
                    Span::raw("  "),
                    Span::styled(short(f), text()),
                    Span::styled(format!("  {}", ids.join(" ")), dim()),
                ],
                w,
            )));
        }
        out.push(Line::default());
    }
    let syms: Vec<&str> = m.symbols.iter().filter(|s| s.mechanical).map(|s| s.name.as_str()).collect();
    if !syms.is_empty() {
        out.push(Line::from(Span::styled(
            format!("Symbols with only mechanical changes · {}", syms.len()),
            Style::default().fg(LAVENDER).add_modifier(Modifier::BOLD),
        )));
        for l in markdown(&syms.join(", "), w.saturating_sub(2)) {
            let mut spans = vec![Span::raw("  ")];
            spans.extend(l.spans.into_iter().map(|s| s.style(dim())));
            out.push(Line::from(spans));
        }
    }
    out
}

const GROUP_LINES: usize = 6;

pub fn group(
    ws: &Workspace,
    g: &crate::tour::Group,
    w: usize,
    hs: usize,
    findings: &[&Finding],
    cards: &[&str],
) -> Lines {
    let mut out = Vec::new();
    if !g.title.is_empty() {
        out.push(Line::from(vec![
            Span::styled(g.title.clone(), Style::default().fg(LAVENDER).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {} hunks", g.hunks.len()), dim()),
        ]));
    }
    if !g.text.is_empty() {
        out.extend(markdown(&g.text, w));
    }
    for id in &g.hunks {
        let Some(h) = ws.hunk(id) else {
            out.push(Line::from(Span::styled(format!("  {id} not found"), dim())));
            continue;
        };
        let names: Vec<&str> = h
            .symbols
            .iter()
            .filter_map(|s| ws.symbol(s))
            .map(|s| s.name.as_str())
            .collect();
        let end = h.new.0 + h.new.1.saturating_sub(1);
        let hit: Vec<&&Finding> = findings.iter().filter(|f| f.hits(&h.file, h.new.0, end)).collect();
        let mark = match hit.iter().map(|f| f.severity).min() {
            Some(sev) => Span::styled("◆ ", severity_style(sev)),
            None => Span::raw(""),
        };
        out.push(Line::from(truncate(
            vec![
                mark,
                Span::styled(format!("{:<5}", h.id), Style::default().fg(SURFACE)),
                Span::styled(format!("{}:{}", short(&h.file), h.new.0), Style::default().fg(SUBTEXT)),
                Span::styled(
                    if names.is_empty() { String::new() } else { format!("  {}", names.join(", ")) },
                    Style::default().fg(LAVENDER),
                ),
            ],
            w,
        )));
        for f in hit.iter().filter(|f| cards.contains(&f.id.as_str())) {
            out.extend(finding_card(f, vec![Span::raw("     ")], w));
        }
        let lines: Vec<&String> = ws
            .file(&h.file)
            .and_then(|f| f.diff.hunks.iter().find(|x| x.id == h.id))
            .map(|x| x.lines.iter().filter(|l| l.starts_with('+') || l.starts_with('-')).collect())
            .unwrap_or_default();
        for l in lines.iter().take(GROUP_LINES) {
            let (style, bg) = if l.starts_with('+') {
                (Style::default().fg(GREEN), ADD_BG)
            } else {
                (Style::default().fg(RED), DEL_BG)
            };
            let text = l[1..].replace('\t', " ");
            let mut row = vec![
                Span::styled(if hs > 0 { "    «" } else { "     " }, Style::default().fg(PEACH)),
                Span::styled(format!("{} ", &l[..1]), style.bg(bg)),
            ];
            row.extend(skip(vec![Span::styled(text, Style::default().fg(TEXT).bg(bg))], hs));
            out.push(Line::from(truncate(row, w)));
        }
        if lines.len() > GROUP_LINES {
            out.push(Line::from(Span::styled(
                format!("     ⋯ {} more changed lines", lines.len() - GROUP_LINES),
                dim().add_modifier(Modifier::ITALIC),
            )));
        }
    }
    out
}
