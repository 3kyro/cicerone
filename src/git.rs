use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

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
    pub fn generated(&self, paths: &[&str]) -> Result<Vec<String>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = vec!["check-attr", "linguist-generated", "--"];
        args.extend(paths);
        let out = self.git(&args)?;
        Ok(out
            .lines()
            .filter_map(|l| l.strip_suffix(": linguist-generated: true"))
            .chain(paths.iter().copied().filter(|p| is_lock_file(p)))
            .map(str::to_string)
            .collect())
    }

    /// For each `(rev, path)`, the file contents, or `None` if it does not exist.
    /// `rev == None` reads the working tree.
    pub fn show_many(&self, reqs: &[(Option<&str>, &str)]) -> Result<Vec<Option<String>>> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("running git cat-file")?;
        let mut stdin = child.stdin.take().unwrap();
        let in_git: Vec<String> = reqs
            .iter()
            .filter_map(|(rev, path)| rev.map(|r| format!("{r}:{path}\n")))
            .collect();
        let writer = std::thread::spawn(move || -> std::io::Result<()> {
            for q in in_git {
                stdin.write_all(q.as_bytes())?;
            }
            Ok(())
        });
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut result = Vec::with_capacity(reqs.len());
        for (rev, path) in reqs {
            if rev.is_none() {
                result.push(std::fs::read_to_string(self.root.join(path)).ok());
                continue;
            }
            let mut header = String::new();
            out.read_line(&mut header)?;
            let size = header
                .trim_end()
                .rsplit(' ')
                .next()
                .and_then(|n| n.parse::<usize>().ok());
            match size {
                Some(n) if !header.trim_end().ends_with(" missing") => {
                    let mut buf = vec![0; n + 1];
                    out.read_exact(&mut buf)?;
                    buf.pop();
                    result.push(Some(String::from_utf8_lossy(&buf).into_owned()));
                }
                _ => result.push(None),
            }
        }
        writer.join().expect("cat-file writer")?;
        child.wait()?;
        Ok(result)
    }

    /// `(word, "file:line")` for each whole-word occurrence of any of `words`
    /// in files that match `pathspecs`.
    pub fn grep_words(
        &self,
        rev: Option<&str>,
        words: &[&str],
        pathspecs: &[String],
    ) -> Result<Vec<(String, String)>> {
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let mut args: Vec<&str> = vec!["grep", "-n", "-o", "-w", "-F", "-I"];
        for w in words {
            args.extend(["-e", w]);
        }
        args.extend(rev);
        args.push("--");
        args.extend(pathspecs.iter().map(String::as_str));
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(&args)
            .output()
            .context("running git grep")?;
        let prefix = rev.map(|r| format!("{r}:")).unwrap_or_default();
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let l = l.strip_prefix(&prefix).unwrap_or(l);
                let mut parts = l.splitn(3, ':');
                let (file, line, word) = (parts.next()?, parts.next()?, parts.next()?);
                Some((word.to_string(), format!("{file}:{line}")))
            })
            .collect())
    }
}

fn is_lock_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(
        name,
        "package-lock.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "Cargo.lock"
            | "flake.lock"
            | "cabal.project.freeze"
            | "elm-stuff"
    ) || name.ends_with(".min.js")
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
