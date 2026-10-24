use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// What to compare. `head == None` means the working tree.
#[derive(Debug, Clone)]
pub struct Range {
    pub base: String,
    pub head: Option<String>,
}

pub struct Repo {
    pub root: PathBuf,
}

impl Repo {
    pub fn discover(dir: &Path) -> Result<Repo> {
        let out = git_in(dir, &["rev-parse", "--show-toplevel"])?;
        Ok(Repo {
            root: PathBuf::from(out.trim()),
        })
    }

    pub fn git(&self, args: &[&str]) -> Result<String> {
        git_in(&self.root, args)
    }

    pub fn resolve(&self, spec: Option<&str>) -> Result<Range> {
        let rev = |r: &str| -> Result<String> {
            Ok(self
                .git(&["rev-parse", "--short", &format!("{r}^{{commit}}")])?
                .trim()
                .to_string())
        };
        match spec {
            None => Ok(Range {
                base: rev("HEAD")?,
                head: None,
            }),
            Some(s) => {
                if let Some((a, b)) = s.split_once("...") {
                    let b = if b.is_empty() { "HEAD" } else { b };
                    let a = if a.is_empty() { "HEAD" } else { a };
                    let mb = self.git(&["merge-base", a, b])?;
                    Ok(Range {
                        base: rev(mb.trim())?,
                        head: Some(rev(b)?),
                    })
                } else if let Some((a, b)) = s.split_once("..") {
                    let b = if b.is_empty() { "HEAD" } else { b };
                    Ok(Range {
                        base: rev(a)?,
                        head: Some(rev(b)?),
                    })
                } else {
                    Ok(Range {
                        base: rev(&format!("{s}^"))?,
                        head: Some(rev(s)?),
                    })
                }
            }
        }
    }

    pub fn diff(&self, range: &Range) -> Result<String> {
        let mut args = vec![
            "-c",
            "core.quotePath=false",
            "diff",
            "-M",
            "--no-color",
            "--no-ext-diff",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "-U3",
            &range.base,
        ];
        if let Some(h) = &range.head {
            args.push(h);
        }
        self.git(&args)
    }

    pub fn show(&self, rev: Option<&str>, path: &str) -> Result<String> {
        match rev {
            Some(r) => self.git(&["show", &format!("{r}:{path}")]),
            None => std::fs::read_to_string(self.root.join(path))
                .with_context(|| format!("reading {path}")),
        }
    }
}

fn git_in(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("running git")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
