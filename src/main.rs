mod agent;
mod app;
mod chat;
mod diff;
mod git;
mod highlight;
mod lens;
mod manifest;
mod mechanical;
mod plan;
mod review;
mod syntax;
mod theme;
mod tour;
mod view;

use anyhow::{Context, Result, bail};
use manifest::{Change, Manifest, Place};
use std::path::PathBuf;

const USAGE: &str = "\
usage: cicerone [review] [--tour <file>] [--no-agent] [--replan] [--model <m>] [-C <dir>] [<range>]
       cicerone tour [-C <dir>] [<range>]
       cicerone plan [--model <m>] [-C <dir>] [<range>]
       cicerone manifest [--json] [-C <dir>] [<range>]

review     walk through the change in the terminal; an agent plans the tour
           (cached per range), --no-agent uses the automatic tour
tour       print the automatic tour as JSON, a template for --tour
plan       let the agent plan the tour, cache it, and print it as JSON
manifest   print the changed symbols, hunks, and mechanical patterns

<range>   A..B     changes from A to B
          A...B    changes on B since its merge base with A
          <rev>    a single commit
          (none)   the working tree against HEAD";

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match args.first().map(String::as_str) {
        Some("review" | "tour" | "manifest" | "plan") => args.remove(0),
        _ => "review".to_string(),
    };
    let (mut json, mut dir, mut spec, mut tour_file) = (false, PathBuf::from("."), None, None);
    let mut snapshot: Option<String> = None;
    let (mut no_agent, mut replan, mut model) = (false, false, None);
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "-C" => dir = it.next().map(PathBuf::from).unwrap_or(dir),
            "--tour" => tour_file = it.next().map(PathBuf::from),
            "--snapshot" => snapshot = it.next(),
            "--no-agent" => no_agent = true,
            "--replan" => replan = true,
            "--model" => model = it.next(),
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            s if s.starts_with('-') => bail!("unknown flag {s}\n\n{USAGE}"),
            _ => spec = Some(a),
        }
    }
    let tour: Option<tour::Tour> = match &tour_file {
        Some(p) => Some(
            serde_json::from_str(&std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?)
                .with_context(|| format!("parsing {}", p.display()))?,
        ),
        None => None,
    };
    let repo = git::Repo::discover(&dir)?;
    let spec = spec.or_else(|| tour.as_ref().and_then(|t| t.range.clone()));
    let range = repo.resolve(spec.as_deref())?;
    std::thread::spawn(highlight::warm);
    let mut ws = manifest::build(&repo, &range)?;
    let mut _tmp: Option<tempfile::TempDir> = None;
    if matches!(cmd.as_str(), "review" | "plan") && !no_agent && snapshot.is_none() {
        let dir = match range.head.as_deref().and_then(|h| plan::cache_dir(&repo.root, &range.base, h)) {
            Some(d) => Some(d.join("files")),
            None => tempfile::Builder::new()
                .prefix("cicerone-")
                .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
                .tempdir()
                .ok()
                .map(|t| _tmp.insert(t).path().to_path_buf()),
        };
        if let Some(dir) = dir
            && plan::snapshot(&ws, &dir).is_ok()
        {
            ws.snapshot = Some(dir);
        }
    }
    match cmd.as_str() {
        "manifest" if json => println!("{}", serde_json::to_string_pretty(&ws.manifest)?),
        "manifest" => print_text(&ws.manifest),
        "tour" => println!("{}", serde_json::to_string_pretty(&tour::auto(&ws))?),
        "plan" => {
            let cache = range
                .head
                .as_deref()
                .and_then(|h| plan::cache_dir(&repo.root, &range.base, h));
            let (mut agent, rx, _) = start_planner(&repo, &ws, cache.as_deref(), model.as_deref())?;
            let t = plan::run_headless(&ws, &mut agent, rx, cache.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&t)?);
        }
        _ if snapshot.is_some() => {
            let keys = snapshot.unwrap_or_default();
            let cache = range
                .head
                .as_deref()
                .and_then(|h| plan::cache_dir(&repo.root, &range.base, h));
            let findings = review::load(cache.as_deref()).unwrap_or_default();
            let app = app::App::new(&ws, tour::auto(&ws), tour)
                .with_config(None, cache.clone())
                .with_findings(findings);
            print!("{}", app.snapshot(&keys, 120, 40)?);
        }
        _ => {
            let auto = tour::auto(&ws);
            let cache = range
                .head
                .as_deref()
                .and_then(|h| plan::cache_dir(&repo.root, &range.base, h));
            let cached: Option<tour::Tour> = cache
                .as_ref()
                .filter(|_| !replan)
                .and_then(|d| std::fs::read_to_string(d.join("tour.json")).ok())
                .and_then(|j| serde_json::from_str::<tour::Tour>(&j).ok())
                .map(|mut t| {
                    plan::refresh(&mut t, &ws);
                    t
                });
            let session = cache
                .as_ref()
                .and_then(|d| std::fs::read_to_string(d.join("session")).ok())
                .map(|s| s.trim().to_string());
            let config = (!no_agent).then(|| app::AgentConfig {
                root: repo.root.clone(),
                model: model.clone(),
                session: session.filter(|_| tour.is_none()),
            });
            let findings = review::load(cache.as_deref());
            let app = if tour.is_some() || no_agent || cached.is_some() {
                let resumable = cached.is_some() && config.is_some();
                let app = app::App::new(&ws, auto, tour.or(cached)).with_config(config, cache.clone());
                match findings {
                    Some(f) => app.with_findings(f),
                    None if resumable => app.review_now(),
                    None => app,
                }
            } else {
                let (a, rx, session) = start_planner(&repo, &ws, cache.as_deref(), model.as_deref())?;
                let config = config.map(|c| app::AgentConfig {
                    session: Some(session),
                    ..c
                });
                app::App::new(&ws, auto, None)
                    .with_config(config, cache.clone())
                    .planning_with(a, rx)
            };
            let mut terminal = ratatui::init();
            let _ = ratatui::crossterm::execute!(std::io::stdout(), ratatui::crossterm::event::EnableMouseCapture);
            let res = app.run(&mut terminal);
            let _ = ratatui::crossterm::execute!(std::io::stdout(), ratatui::crossterm::event::DisableMouseCapture);
            ratatui::restore();
            res?;
        }
    }
    Ok(())
}

fn start_planner(
    repo: &git::Repo,
    ws: &manifest::Workspace,
    cache: Option<&std::path::Path>,
    model: Option<&str>,
) -> Result<(agent::Agent, std::sync::mpsc::Receiver<agent::Event>, String)> {
    let session = agent::new_session_id();
    if let Some(d) = cache {
        std::fs::create_dir_all(d)?;
        std::fs::write(d.join("session"), &session)?;
    }
    let log = cache.map(|d| d.join("agent.log"));
    let (tx, rx) = std::sync::mpsc::channel();
    let mut a = agent::Agent::start(
        agent::Options {
            cwd: &repo.root,
            session: &session,
            resume: false,
            system: plan::SYSTEM,
            model,
            log: log.as_deref(),
            files: ws.snapshot.as_deref(),
        },
        tx,
    )?;
    a.send(&plan::prompt(ws))?;
    Ok((a, rx, session))
}

pub fn clean(s: &str) -> String {
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
