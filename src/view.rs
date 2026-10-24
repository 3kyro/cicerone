use crate::lens::{LensView, RowKind, kind_label};
use crate::manifest::{Change, Symbol, Workspace};
use crate::mechanical::Mechanical;
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

pub fn lens(v: &LensView, focused: bool, w: usize) -> Lines {
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

    let code_w = w.saturating_sub(9);
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
        let mut line = vec![
            Span::styled("│", frame),
            Span::styled(format!("{num} "), if row.focus { Style::default().fg(MAUVE) } else { dim() }),
            Span::styled(format!("{mark} "), mark_style.patch(base)),
        ];
        match &row.kind {
            RowKind::Hole { label, count, changed } => {
                let text = if label.is_empty() {
                    format!("⋯ {count} line{}", if *count == 1 { "" } else { "s" })
                } else {
                    format!("⋯ {label}")
                };
                line.push(Span::raw(row.text.clone()));
                line.push(Span::styled(text, dim().add_modifier(Modifier::ITALIC)));
                if *changed {
                    line.push(Span::styled("  ● changes", Style::default().fg(PEACH)));
                }
            }
            _ => {
                let code = truncate(code_spans(&row.text, &row.spans, base), code_w);
                let used = width(&code);
                line.extend(code);
                if bg.is_some() && used < code_w {
                    line.push(Span::styled(" ".repeat(code_w - used), base));
                }
            }
        }
        out.push(Line::from(line));
        if let Some(n) = &row.note {
            out.push(Line::from(truncate(
                vec![
                    Span::styled("│", frame),
                    Span::raw("       "),
                    Span::styled("╰─ ✦ ", Style::default().fg(PEACH)),
                    Span::styled(n.clone(), Style::default().fg(PEACH).add_modifier(Modifier::ITALIC)),
                ],
                w,
            )));
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
        out.push(Line::from(truncate(
            vec![
                Span::styled("⚠ ", Style::default().fg(YELLOW)),
                Span::styled(format!("old name still used in {} places: ", p.leftovers.len()), text()),
                Span::styled(first.join(", "), dim()),
            ],
            w,
        )));
    }
    out.push(Line::from(Span::styled(
        "The first hunk below is one example. The others are the same substitution.",
        dim(),
    )));
    out
}

pub fn mechanical(ws: &Workspace, w: usize) -> Lines {
    let m = &ws.manifest;
    let mut out = Vec::new();
    for (title, want) in [
        ("Formatting", Mechanical::Formatting),
        ("Imports", Mechanical::Imports),
        ("Generated", Mechanical::Generated),
    ] {
        let hs: Vec<_> = m.hunks.iter().filter(|h| h.mechanical.as_ref() == Some(&want)).collect();
        if hs.is_empty() {
            continue;
        }
        out.push(Line::from(Span::styled(
            format!("{title} · {} hunks", hs.len()),
            Style::default().fg(LAVENDER).add_modifier(Modifier::BOLD),
        )));
        let mut files: Vec<(&str, Vec<&str>)> = Vec::new();
        for h in hs {
            match files.iter_mut().find(|(f, _)| *f == h.file) {
                Some((_, v)) => v.push(&h.id),
                None => files.push((&h.file, vec![&h.id])),
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

pub fn group(ws: &Workspace, g: &crate::tour::Group, w: usize) -> Lines {
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
        out.push(Line::from(truncate(
            vec![
                Span::styled(format!("{:<5}", h.id), Style::default().fg(SURFACE)),
                Span::styled(format!("{}:{}", short(&h.file), h.new.0), Style::default().fg(SUBTEXT)),
                Span::styled(
                    if names.is_empty() { String::new() } else { format!("  {}", names.join(", ")) },
                    Style::default().fg(LAVENDER),
                ),
            ],
            w,
        )));
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
            out.push(Line::from(truncate(
                vec![
                    Span::raw("     "),
                    Span::styled(format!("{} ", &l[..1]), style.bg(bg)),
                    Span::styled(text, Style::default().fg(TEXT).bg(bg)),
                ],
                w,
            )));
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
