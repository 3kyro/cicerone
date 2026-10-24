use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hunk {
    pub id: String,
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    pub lines: Vec<String>,
}

impl Hunk {
    pub fn added(&self) -> usize {
        self.lines.iter().filter(|l| l.starts_with('+')).count()
    }

    pub fn removed(&self) -> usize {
        self.lines.iter().filter(|l| l.starts_with('-')).count()
    }

    /// 1-based line numbers of the changed lines on each side.
    pub fn changed_lines(&self) -> (Vec<usize>, Vec<usize>) {
        let (mut old, mut new) = (self.old_start, self.new_start);
        let (mut olds, mut news) = (Vec::new(), Vec::new());
        for l in &self.lines {
            match l.as_bytes().first() {
                Some(b'-') => {
                    olds.push(old);
                    old += 1;
                }
                Some(b'+') => {
                    news.push(new);
                    new += 1;
                }
                Some(b'\\') => {}
                _ => {
                    old += 1;
                    new += 1;
                }
            }
        }
        (olds, news)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    pub fn base_path(&self) -> &str {
        self.old_path.as_deref().unwrap_or(&self.path)
    }
}

pub fn parse(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut next_id = 1;
    let (mut old_left, mut new_left) = (0usize, 0usize);
    for line in text.lines() {
        if old_left + new_left > 0
            && let Some(h) = files.last_mut().and_then(|f| f.hunks.last_mut())
        {
            match line.as_bytes().first() {
                Some(b'-') => old_left = old_left.saturating_sub(1),
                Some(b'+') => new_left = new_left.saturating_sub(1),
                Some(b'\\') => {}
                _ => {
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                }
            }
            h.lines.push(line.to_string());
            continue;
        }
        if line.starts_with('\\')
            && let Some(h) = files.last_mut().and_then(|f| f.hunks.last_mut())
        {
            h.lines.push(line.to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let path = rest
                .split_once(" b/")
                .map(|(_, b)| b.to_string())
                .unwrap_or_default();
            files.push(FileDiff {
                path,
                old_path: None,
                status: FileStatus::Modified,
                binary: false,
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(file) = files.last_mut() else { continue };
        if line.starts_with("new file mode") {
            file.status = FileStatus::Added;
        } else if line.starts_with("deleted file mode") {
            file.status = FileStatus::Deleted;
        } else if let Some(p) = line.strip_prefix("rename from ") {
            file.old_path = Some(p.to_string());
            file.status = FileStatus::Renamed;
        } else if let Some(p) = line.strip_prefix("rename to ") {
            file.path = p.to_string();
        } else if let Some(p) = line.strip_prefix("+++ b/") {
            file.path = p.to_string();
        } else if let Some(p) = line.strip_prefix("--- a/")
            && file.status == FileStatus::Deleted
        {
            file.path = p.to_string();
        } else if line.starts_with("Binary files ") {
            file.binary = true;
        } else if let Some(h) = parse_hunk_header(line) {
            (old_left, new_left) = (h.old_len, h.new_len);
            file.hunks.push(Hunk {
                id: format!("H{next_id}"),
                lines: Vec::new(),
                ..h
            });
            next_id += 1;
        }
    }
    files
}

fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@ -")?;
    let (ranges, _) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let range = |s: &str| -> Option<(usize, usize)> {
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (old_start, old_len) = range(old)?;
    let (new_start, new_len) = range(new)?;
    Some(Hunk {
        id: String::new(),
        old_start,
        old_len,
        new_start,
        new_len,
        lines: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
diff --git a/src/A.hs b/src/A.hs
index 1111111..2222222 100644
--- a/src/A.hs
+++ b/src/A.hs
@@ -1,3 +1,4 @@ module A
 module A where
-foo = 1
+foo = 2
+bar = 3
 baz = 4
diff --git a/old.elm b/new.elm
similarity index 90%
rename from old.elm
rename to new.elm
--- a/old.elm
+++ b/new.elm
@@ -5 +5 @@
-x
+y
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
--- a line that starts with dashes
";

    #[test]
    fn parses_files_and_hunks() {
        let files = parse(SAMPLE);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "src/A.hs");
        assert_eq!(files[0].hunks[0].id, "H1");
        assert_eq!(files[0].hunks[0].added(), 2);
        assert_eq!(files[0].hunks[0].removed(), 1);
        assert_eq!(files[0].hunks[0].changed_lines(), (vec![2], vec![2, 3]));
        assert_eq!(files[1].status, FileStatus::Renamed);
        assert_eq!(files[1].old_path.as_deref(), Some("old.elm"));
        assert_eq!(files[1].path, "new.elm");
        assert_eq!(files[1].hunks[0].id, "H2");
        assert_eq!(files[2].status, FileStatus::Deleted);
        assert_eq!(files[2].path, "gone.txt");
        assert_eq!(files[2].hunks[0].lines, vec!["--- a line that starts with dashes"]);
    }
}
