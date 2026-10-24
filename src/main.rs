mod diff;
mod git;
mod manifest;
mod mechanical;
mod syntax;

use anyhow::{Result, bail};
use manifest::{Change, Manifest, Place};
use std::path::PathBuf;

const USAGE: &str = "\
usage: cicerone manifest [--json] [-C <dir>] [<range>]

<range>   A..B     changes from A to B
          A...B    changes on B since its merge base with A
          <rev>    a single commit
          (none)   the working tree against HEAD";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(cmd) = args.next() else {
        bail!("{USAGE}");
    };
    if cmd != "manifest" {
        bail!("{USAGE}");
    }
    let (mut json, mut dir, mut spec) = (false, PathBuf::from("."), None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--json" => json = true,
            "-C" => dir = args.next().map(PathBuf::from).unwrap_or(dir),
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ => spec = Some(a),
        }
    }
    let repo = git::Repo::discover(&dir)?;
    let range = repo.resolve(spec.as_deref())?;
    let m = manifest::build(&repo, &range)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&m)?);
    } else {
        print_text(&m);
    }
    Ok(())
}

fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { char::REPLACEMENT_CHARACTER } else { c })
        .collect()
}

fn label<T: serde::Serialize>(v: T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn print_text(m: &Manifest) {
    let hunks: usize = m.files.iter().map(|f| f.hunks.len()).sum();
    println!(
        "{}..{} · {} files · {} hunks · {} symbols\n",
        clean(&m.base),
        clean(&m.head),
        m.files.len(),
        hunks,
        m.symbols.len()
    );
    let symbol = |id: &String| m.symbols.iter().find(|s| &s.id == id).unwrap();
    let (core, mech): (Vec<_>, Vec<_>) = m.order.iter().map(symbol).partition(|s| !s.mechanical);
    let print_symbol = |s: &manifest::Symbol| {
        let mark = match s.change {
            Change::Added => "+",
            Change::Removed => "-",
            Change::SignatureChanged => "±",
            Change::BodyChanged => "~",
            Change::DocChanged => "d",
            Change::Formatting => "f",
        };
        println!(
            "  {:<4} {} {:<9} {:<30} {}:{}-{}  {}{}",
            s.id,
            mark,
            label(s.kind),
            clean(&s.name),
            clean(&s.file),
            s.lines.0,
            s.lines.1,
            s.hunks.join(" "),
            if s.uses.is_empty() {
                String::new()
            } else {
                format!("  uses {}", s.uses.join(" "))
            }
        );
        if s.change == Change::SignatureChanged {
            if let Some(o) = &s.old_signature {
                println!("         was {}", clean(&syntax::normalize(o)));
            }
            if let Some(n) = &s.signature {
                println!("         now {}", clean(&syntax::normalize(n)));
            }
        }
    };
    if !core.is_empty() {
        println!("Symbols, names before uses");
        core.into_iter().for_each(print_symbol);
        println!();
    }
    let loose: Vec<_> = m
        .hunks
        .iter()
        .filter(|h| h.place != Place::Symbols && h.mechanical.is_none())
        .collect();
    if !loose.is_empty() {
        println!("Hunks outside symbols");
        for h in loose {
            println!(
                "  {:<4} {:<8} {}:{}  +{} -{}",
                h.id,
                label(h.place),
                clean(&h.file),
                h.new.0,
                h.added,
                h.removed
            );
        }
        println!();
    }
    let of_kind = |k: &str| -> Vec<&str> {
        m.hunks
            .iter()
            .filter(|h| h.mechanical.as_ref().is_some_and(|x| label_kind(x) == k))
            .map(|h| h.id.as_str())
            .collect()
    };
    let (gen_, fmt, imp) = (of_kind("generated"), of_kind("formatting"), of_kind("imports"));
    if !m.patterns.is_empty() || !gen_.is_empty() || !fmt.is_empty() || !imp.is_empty() || !mech.is_empty() {
        println!("Mechanical");
        for p in &m.patterns {
            let subs: Vec<String> = p
                .substitutions
                .iter()
                .map(|s| format!("{} → {}", show(&s.old), show(&s.new)))
                .collect();
            println!(
                "  {:<4} {}   {} sites, {} hunks, {} files  ✓",
                p.id,
                clean(&subs.join(", ")),
                p.sites,
                p.hunks.len(),
                p.files
            );
            println!("         {}", p.hunks.join(" "));
            for o in &p.outliers {
                println!("       ⚠ outlier {}: {}", o.hunk, clean(&o.reason));
            }
            if !p.leftovers.is_empty() {
                let more = if p.leftovers.len() > 3 {
                    format!(" …{} total", p.leftovers.len())
                } else {
                    String::new()
                };
                println!(
                    "       ⚠ old name still in head: {}{}",
                    clean(&p.leftovers[..p.leftovers.len().min(3)].join(" ")),
                    more
                );
            }
        }
        for (name, ids) in [("formatting", &fmt), ("imports", &imp), ("generated", &gen_)] {
            if !ids.is_empty() {
                println!("  {:<11} {}", name, ids.join(" "));
            }
        }
        if !mech.is_empty() {
            let ids: Vec<&str> = mech.iter().map(|s| s.id.as_str()).collect();
            println!("  symbols     {}", ids.join(" "));
        }
        println!();
    }
    println!("Files");
    for f in &m.files {
        println!(
            "  {:<9} {}  +{} -{}  {}",
            label(f.status),
            clean(&f.path),
            f.added,
            f.removed,
            f.hunks.join(" ")
        );
    }
}

fn label_kind(k: &mechanical::Mechanical) -> &'static str {
    match k {
        mechanical::Mechanical::Generated => "generated",
        mechanical::Mechanical::Formatting => "formatting",
        mechanical::Mechanical::Imports => "imports",
        mechanical::Mechanical::Pattern(_) => "pattern",
    }
}

fn show(s: &str) -> String {
    if s.is_empty() { "∅".into() } else { format!("`{s}`") }
}
