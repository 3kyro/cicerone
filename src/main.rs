mod diff;
mod git;
mod manifest;
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

fn print_text(m: &Manifest) {
    let hunks: usize = m.files.iter().map(|f| f.hunks.len()).sum();
    println!(
        "{}..{} · {} files · {} hunks · {} symbols\n",
        m.base,
        m.head,
        m.files.len(),
        hunks,
        m.symbols.len()
    );
    if !m.order.is_empty() {
        println!("Symbols, names before uses");
        for id in &m.order {
            let s = m.symbols.iter().find(|s| &s.id == id).unwrap();
            let mark = match s.change {
                Change::Added => "+",
                Change::Removed => "-",
                Change::SignatureChanged => "±",
                Change::BodyChanged => "~",
                Change::DocChanged => "d",
                Change::Formatting => "f",
            };
            let kind = serde_json::to_value(s.kind).unwrap();
            println!(
                "  {:<4} {} {:<9} {:<30} {}:{}-{}  {}{}",
                s.id,
                mark,
                kind.as_str().unwrap_or(""),
                s.name,
                s.file,
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
                    println!("         was {}", syntax::normalize(o));
                }
                if let Some(n) = &s.signature {
                    println!("         now {}", syntax::normalize(n));
                }
            }
        }
        println!();
    }
    let loose: Vec<_> = m.hunks.iter().filter(|h| h.place != Place::Symbols).collect();
    if !loose.is_empty() {
        println!("Hunks outside symbols");
        for h in loose {
            let place = serde_json::to_value(h.place).unwrap();
            println!(
                "  {:<4} {:<8} {}:{}  +{} -{}",
                h.id,
                place.as_str().unwrap_or(""),
                h.file,
                h.new.0,
                h.added,
                h.removed
            );
        }
        println!();
    }
    println!("Files");
    for f in &m.files {
        let status = serde_json::to_value(f.status).unwrap();
        println!(
            "  {:<9} {}  +{} -{}  {}",
            status.as_str().unwrap_or(""),
            f.path,
            f.added,
            f.removed,
            f.hunks.join(" ")
        );
    }
}
