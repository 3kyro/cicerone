use crate::manifest::Workspace;
use crate::review::{Finding, Severity};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identity {
    Claude,
    Me,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Comment,
    RequestChanges,
    Approve,
}

impl Event {
    fn api(self) -> &'static str {
        match self {
            Event::Comment => "COMMENT",
            Event::RequestChanges => "REQUEST_CHANGES",
            Event::Approve => "APPROVE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub path: String,
    pub lines: (usize, usize),
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub identity: Identity,
    pub event: Event,
    pub body: String,
    pub comments: Vec<Comment>,
}

const CLAUDE_NOTE: &str = "🤖 *This review was written by Claude, through the cicerone review tool.*";
const CLAUDE_PREFIX: &str = "🤖 **Claude:** ";

/// The head lines a review comment can sit on: the new side of each hunk.
fn diff_lines(ws: &Workspace) -> BTreeMap<&str, Vec<(usize, usize)>> {
    let mut out: BTreeMap<&str, Vec<(usize, usize)>> = BTreeMap::new();
    for h in &ws.manifest.hunks {
        if h.new.1 > 0 {
            out.entry(h.file.as_str())
                .or_default()
                .push((h.new.0, h.new.0 + h.new.1 - 1));
        }
    }
    out
}

/// The part of `lines` that one hunk of `file` shows, ending on the
/// finding's last line in a hunk.
fn anchor(ranges: &BTreeMap<&str, Vec<(usize, usize)>>, file: &str, lines: (usize, usize)) -> Option<(usize, usize)> {
    ranges.get(file)?.iter().rev().find_map(|&(a, b)| {
        let end = lines.1.min(b);
        let start = lines.0.max(a);
        (start <= end).then_some((start, end))
    })
}

fn heading(f: &Finding) -> String {
    format!("**{}: {}**", f.severity.label(), f.title)
}

/// The editable review: a header that sets identity and event, the summary
/// body, then one section per inline comment.
pub fn compose(ws: &Workspace, findings: &[Finding], intent: &str, pr: u64) -> String {
    let ranges = diff_lines(ws);
    let open: Vec<&Finding> = {
        let mut v: Vec<&Finding> = findings.iter().filter(|f| !f.withdrawn).collect();
        v.sort_by_key(|f| f.severity);
        v
    };
    let mut inline = Vec::new();
    let mut general = Vec::new();
    for f in &open {
        match anchor(&ranges, &f.file, f.lines) {
            Some(lines) => inline.push((f, lines)),
            None => general.push(f),
        }
    }
    let count = |s: Severity| open.iter().filter(|f| f.severity == s).count();
    let mut counts = Vec::new();
    for (s, one, many) in [
        (Severity::Bug, "bug", "bugs"),
        (Severity::Risk, "risk", "risks"),
        (Severity::Change, "behavior change", "behavior changes"),
        (Severity::Question, "question", "questions"),
        (Severity::Nit, "nit", "nits"),
    ] {
        match count(s) {
            0 => {}
            1 => counts.push(format!("1 {one}")),
            n => counts.push(format!("{n} {many}")),
        }
    }

    let mut s = String::new();
    let _ = writeln!(s, "# cicerone: review for PR #{pr}. Edit it, then save and quit to post it.");
    let _ = writeln!(s, "# Lines that start with # are left out. Delete everything to cancel.");
    let _ = writeln!(s, "#");
    let _ = writeln!(s, "# as:    claude = marked as written by Claude · me = plain, in your name");
    let _ = writeln!(s, "# event: comment · request_changes · approve");
    let _ = writeln!(s, "# Each \"=== comment <path>:<line>\" or \"<path>:<start>-<end>\" is one inline comment;");
    let _ = writeln!(s, "# its lines must be in the diff of the PR.");
    let _ = writeln!(s, "as: claude");
    let _ = writeln!(s, "event: comment\n");
    let _ = writeln!(s, "=== body");
    if !intent.trim().is_empty() {
        let _ = writeln!(s, "{}\n", intent.trim());
    }
    if !counts.is_empty() {
        let _ = writeln!(s, "Findings: {}.", counts.join(", "));
    }
    for f in &general {
        let _ = writeln!(s, "\n{} (`{}:{}-{}`)\n\n{}", heading(f), f.file, f.lines.0, f.lines.1, f.body.trim());
    }
    for (f, (a, b)) in inline {
        let place = if a == b { format!("{a}") } else { format!("{a}-{b}") };
        let _ = writeln!(s, "\n=== comment {}:{place}\n{}\n\n{}", f.file, heading(f), f.body.trim());
    }
    s
}

pub fn parse(text: &str) -> Result<Option<Draft>> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
    if lines.iter().all(|l| l.trim().is_empty()) {
        return Ok(None);
    }
    let (mut identity, mut event) = (Identity::Claude, Event::Comment);
    let mut body = String::new();
    let mut comments: Vec<Comment> = Vec::new();
    let mut section: Option<Option<Comment>> = None;
    let flush = |section: &mut Option<Option<Comment>>, buf: &mut String, body: &mut String, comments: &mut Vec<Comment>| {
        let text = std::mem::take(buf).trim().to_string();
        match section.take() {
            Some(None) => *body = text,
            Some(Some(mut c)) if !text.is_empty() => {
                c.body = text;
                comments.push(c);
            }
            _ => {}
        }
    };
    let mut buf = String::new();
    for l in lines {
        if let Some(rest) = l.strip_prefix("=== ") {
            flush(&mut section, &mut buf, &mut body, &mut comments);
            let rest = rest.trim();
            if rest == "body" {
                section = Some(None);
            } else if let Some(target) = rest.strip_prefix("comment ") {
                let (path, place) = target
                    .trim()
                    .rsplit_once(':')
                    .with_context(|| format!("comment target needs <path>:<line>: {target}"))?;
                let (a, b) = match place.split_once('-') {
                    Some((a, b)) => (a.trim().parse()?, b.trim().parse()?),
                    None => {
                        let n: usize = place.trim().parse()?;
                        (n, n)
                    }
                };
                section = Some(Some(Comment {
                    path: path.to_string(),
                    lines: (a.min(b), a.max(b)),
                    body: String::new(),
                }));
            } else {
                bail!("unknown section: === {rest}");
            }
            continue;
        }
        if section.is_none() {
            if let Some(v) = l.strip_prefix("as:") {
                identity = match v.trim() {
                    "claude" => Identity::Claude,
                    "me" => Identity::Me,
                    other => bail!("as: must be claude or me, not {other}"),
                };
            } else if let Some(v) = l.strip_prefix("event:") {
                event = match v.trim() {
                    "comment" => Event::Comment,
                    "request_changes" => Event::RequestChanges,
                    "approve" => Event::Approve,
                    other => bail!("event: must be comment, request_changes, or approve, not {other}"),
                };
            }
            continue;
        }
        buf.push_str(l);
        buf.push('\n');
    }
    flush(&mut section, &mut buf, &mut body, &mut comments);
    if body.is_empty() && comments.is_empty() {
        return Ok(None);
    }
    Ok(Some(Draft {
        identity,
        event,
        body,
        comments,
    }))
}

/// Checks that each comment sits inside one hunk of the change.
pub fn check(draft: &Draft, ws: &Workspace) -> Result<()> {
    let ranges = diff_lines(ws);
    for c in &draft.comments {
        let inside = ranges
            .get(c.path.as_str())
            .is_some_and(|rs| rs.iter().any(|&(a, b)| a <= c.lines.0 && c.lines.1 <= b));
        if !inside {
            bail!(
                "{}:{}-{} is not inside one hunk of the diff; move it into the body or fix the lines",
                c.path,
                c.lines.0,
                c.lines.1
            );
        }
    }
    Ok(())
}

pub fn payload(draft: &Draft, commit: &str) -> Value {
    let label = |text: &str, prefix: &str| match draft.identity {
        Identity::Claude => format!("{prefix}{text}"),
        Identity::Me => text.to_string(),
    };
    let body = match draft.identity {
        Identity::Claude => format!("{CLAUDE_NOTE}\n\n{}", draft.body).trim().to_string(),
        Identity::Me => draft.body.clone(),
    };
    let comments: Vec<Value> = draft
        .comments
        .iter()
        .map(|c| {
            let mut v = json!({
                "path": c.path,
                "line": c.lines.1,
                "side": "RIGHT",
                "body": label(&c.body, CLAUDE_PREFIX),
            });
            if c.lines.0 < c.lines.1 {
                v["start_line"] = json!(c.lines.0);
                v["start_side"] = json!("RIGHT");
            }
            v
        })
        .collect();
    json!({
        "commit_id": commit,
        "event": draft.event.api(),
        "body": body,
        "comments": comments,
    })
}

/// The open PR whose head is `commit`.
pub fn find_pr(root: &Path, commit: &str) -> Option<u64> {
    let out = Command::new("gh")
        .current_dir(root)
        .args(["pr", "list", "--state", "open", "--search", commit, "--json", "number,headRefOid"])
        .output()
        .ok()?;
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    v.as_array()?
        .iter()
        .find(|p| p["headRefOid"].as_str() == Some(commit))
        .and_then(|p| p["number"].as_u64())
}

/// Posts the review and returns its URL.
pub fn post(root: &Path, pr: u64, payload: &Value) -> Result<String> {
    let mut child = Command::new("gh")
        .current_dir(root)
        .args(["api", "--method", "POST"])
        .arg(format!("repos/{{owner}}/{{repo}}/pulls/{pr}/reviews"))
        .args(["--input", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running gh")?;
    {
        use std::io::Write as _;
        child.stdin.take().unwrap().write_all(payload.to_string().as_bytes())?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "gh api failed: {}{}",
            String::from_utf8_lossy(&out.stderr).trim(),
            String::from_utf8_lossy(&out.stdout).trim()
        );
    }
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    Ok(v["html_url"].as_str().unwrap_or("").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{FileDiff, FileStatus};
    use crate::manifest::{FileData, HunkEntry, Manifest, Place};

    fn ws() -> Workspace {
        Workspace {
            manifest: Manifest {
                base: "a".into(),
                head: "b".into(),
                files: Vec::new(),
                symbols: Vec::new(),
                hunks: vec![HunkEntry {
                    id: "H1".into(),
                    file: "A.hs".into(),
                    old: (10, 3),
                    new: (10, 6),
                    added: 3,
                    removed: 0,
                    place: Place::Symbols,
                    mechanical: None,
                    symbols: Vec::new(),
                }],
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

    fn finding(id: &str, sev: Severity, lines: (usize, usize), withdrawn: bool) -> Finding {
        Finding {
            id: id.into(),
            severity: sev,
            title: format!("title {id}"),
            body: format!("body {id}"),
            file: "A.hs".into(),
            lines,
            withdrawn,
            history: Vec::new(),
        }
    }

    #[test]
    fn compose_then_parse_keeps_inline_and_general_findings() {
        let ws = ws();
        let fs = [
            finding("F1", Severity::Nit, (12, 13), false),
            finding("F2", Severity::Risk, (40, 41), false),
            finding("F3", Severity::Bug, (11, 11), true),
        ];
        let text = compose(&ws, &fs, "Locks the bundle.", 7);
        let d = parse(&text).unwrap().unwrap();
        assert_eq!((d.identity, d.event), (Identity::Claude, Event::Comment));
        assert!(d.body.contains("Locks the bundle.") && d.body.contains("1 risk, 1 nit"));
        assert!(d.body.contains("**RISK: title F2**"), "outside the diff goes to the body");
        assert!(!text.contains("F3"), "withdrawn findings stay out");
        assert_eq!(d.comments.len(), 1);
        assert_eq!(d.comments[0].lines, (12, 13));
        assert!(check(&d, &ws).is_ok());
    }

    #[test]
    fn identity_decides_the_labels() {
        let mut d = Draft {
            identity: Identity::Claude,
            event: Event::RequestChanges,
            body: "sum".into(),
            comments: vec![Comment {
                path: "A.hs".into(),
                lines: (11, 12),
                body: "c".into(),
            }],
        };
        let p = payload(&d, "abc");
        assert!(p["body"].as_str().unwrap().starts_with("🤖"));
        assert_eq!(p["comments"][0]["body"], "🤖 **Claude:** c");
        assert_eq!(p["comments"][0]["start_line"], 11);
        assert_eq!(p["event"], "REQUEST_CHANGES");
        d.identity = Identity::Me;
        let p = payload(&d, "abc");
        assert_eq!(p["body"], "sum");
        assert_eq!(p["comments"][0]["body"], "c");
    }

    #[test]
    fn parse_handles_cancel_and_bad_values() {
        assert!(parse("# only comments\n\n").unwrap().is_none());
        assert!(parse("as: robot\n=== body\nx\n").is_err());
        let d = parse("as: me\nevent: approve\n=== body\nLGTM\n").unwrap().unwrap();
        assert_eq!((d.identity, d.event, d.body.as_str()), (Identity::Me, Event::Approve, "LGTM"));
        let mut bad = d.clone();
        bad.comments.push(Comment {
            path: "A.hs".into(),
            lines: (14, 20),
            body: "x".into(),
        });
        assert!(check(&bad, &ws()).is_err());
    }
}
