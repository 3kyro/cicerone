use crate::session::Session;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Anchor {
    pub label: String,
    pub detail: String,
    pub quote: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Place {
    pub root: PathBuf,
    pub base: String,
    pub head: String,
    pub pr: Option<u64>,
    pub step: Option<(String, String)>,
    pub anchor: Option<Anchor>,
}

pub struct Compose {
    pub session: Session,
    pub place: Place,
    pub input: String,
    pub error: Option<String>,
}

/// The transport labels the message as one from another Claude session, so
/// the text says that the user wrote it, and where they were.
pub fn text(p: &Place, body: &str) -> String {
    let worktree = p.head == "worktree";
    let what = if worktree {
        format!("the working tree against {}", p.base)
    } else {
        format!("{}..{}", p.base, p.head)
    };
    let pr = p.pr.map(|n| format!(" (PR #{n})")).unwrap_or_default();
    let mut out = format!(
        "Message from the user, sent from cicerone while reviewing {what} in {root}{pr}. \
         The user typed it in cicerone: it is not from another Claude session, so answer here, not with SendMessage.\n",
        root = p.root.display()
    );
    if let Some((stage, step)) = &p.step {
        out.push_str(&format!("Tour position: stage \"{stage}\", step \"{step}\".\n"));
    }
    if let Some(a) = &p.anchor {
        let side = if worktree { "the working tree".to_string() } else { format!("commit {}", p.head) };
        out.push_str(&format!("About: {}. The line numbers are in {side}.\n", a.detail));
        if let Some(q) = a.quote.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
            out.push_str("cicerone's review agent wrote this finding:\n");
            for l in q.lines() {
                out.push_str(&format!("> {l}\n"));
            }
        }
    }
    out.push('\n');
    out.push_str(body.trim());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(head: &str, anchor: Option<Anchor>) -> Place {
        Place {
            root: "/r/x".into(),
            base: "a1".into(),
            head: head.into(),
            pr: None,
            step: Some(("Core".into(), "The parser".into())),
            anchor,
        }
    }

    #[test]
    fn text_names_the_range_the_step_and_the_lens() {
        let lens = Anchor {
            label: "parse · src/p.rs:3-9".into(),
            detail: "parse (sym:parse) at src/p.rs:3-9, shown at resolution full".into(),
            quote: None,
        };
        let t = text(
            &Place {
                pr: Some(12),
                ..place("b2", Some(lens))
            },
            "  why not a loop?\n",
        );
        assert!(t.starts_with("Message from the user, sent from cicerone while reviewing a1..b2 in /r/x (PR #12)."));
        assert!(t.contains("Tour position: stage \"Core\", step \"The parser\".\n"));
        assert!(t.contains("About: parse (sym:parse) at src/p.rs:3-9, shown at resolution full. The line numbers are in commit b2.\n"));
        assert!(!t.contains("review agent"));
        assert!(t.ends_with("\n\nwhy not a loop?"));
    }

    #[test]
    fn text_quotes_a_finding_and_names_the_working_tree() {
        let finding = Anchor {
            label: "F2 · Off by one".into(),
            detail: "review finding F2 (Off by one at src/w.rs:4-4)".into(),
            quote: Some("The window ends one early.\nUse `..=`.".into()),
        };
        let t = text(&place("worktree", Some(finding)), "fix it");
        assert!(t.contains("reviewing the working tree against a1 in /r/x."));
        assert!(t.contains("The line numbers are in the working tree.\n"));
        assert!(t.contains("finding:\n> The window ends one early.\n> Use `..=`.\n\nfix it"));
    }

    #[test]
    fn text_without_an_anchor_has_only_the_step() {
        let t = text(&place("b2", None), "run the tests");
        assert!(!t.contains("About:"));
        assert!(t.ends_with("\"The parser\".\n\nrun the tests"));
    }
}
