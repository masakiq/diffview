#[derive(Debug, Clone)]
pub enum DiffLine {
    Context(String),
    Added(String),
    Removed(String),
}

#[derive(Debug, Clone)]
pub struct Hunk {
    #[allow(dead_code)]
    pub header: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Default)]
pub struct FileDiff {
    #[allow(dead_code)]
    pub path: String,
    pub is_binary: bool,
    pub hunks: Vec<Hunk>,
}

fn is_binary_diff_text(diff_text: &str) -> bool {
    diff_text.lines().any(|line| {
        (line.starts_with("Binary files ") && line.ends_with(" differ"))
            || line == "GIT binary patch"
    })
}

pub fn parse_diff(diff_text: &str) -> FileDiff {
    if is_binary_diff_text(diff_text) {
        return FileDiff {
            path: String::new(),
            is_binary: true,
            hunks: vec![],
        };
    }

    let mut hunks: Vec<Hunk> = Vec::new();
    let mut current_hunk: Option<Hunk> = None;
    let mut path = String::new();

    for line in diff_text.lines() {
        if line.starts_with("+++ b/") {
            path = line[6..].to_string();
        } else if line.starts_with("@@") {
            if let Some(hunk) = current_hunk.take() {
                hunks.push(hunk);
            }
            if let Some(hunk) = parse_hunk_header(line) {
                current_hunk = Some(hunk);
            }
        } else if let Some(ref mut hunk) = current_hunk {
            if line.starts_with('+') {
                hunk.lines.push(DiffLine::Added(line[1..].to_string()));
            } else if line.starts_with('-') {
                hunk.lines.push(DiffLine::Removed(line[1..].to_string()));
            } else if line.starts_with(' ') {
                hunk.lines.push(DiffLine::Context(line[1..].to_string()));
            }
        }
    }

    if let Some(hunk) = current_hunk {
        hunks.push(hunk);
    }

    FileDiff {
        path,
        is_binary: false,
        hunks,
    }
}

fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let parts: Vec<&str> = line.splitn(5, ' ').collect();
    if parts.len() < 3 {
        return None;
    }

    let old = parts[1].trim_start_matches('-');
    let new = parts[2].trim_start_matches('+');

    let (old_start, old_count) = parse_range(old);
    let (new_start, new_count) = parse_range(new);

    Some(Hunk {
        header: line.to_string(),
        old_start,
        old_count,
        new_start,
        new_count,
        lines: Vec::new(),
    })
}

fn parse_range(s: &str) -> (u32, u32) {
    if let Some((start, count)) = s.split_once(',') {
        (start.parse().unwrap_or(1), count.parse().unwrap_or(0))
    } else {
        (s.parse().unwrap_or(1), 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_DIFF: &str = r#"diff --git a/src/main.rs b/src/main.rs
index abc..def 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,5 +1,6 @@
 fn main() {
-    println!("hello");
+    println!("hello, world");
+    println!("second line");
 }
"#;

    #[test]
    fn test_parse_hunk() {
        let fd = parse_diff(SAMPLE_DIFF);
        assert_eq!(fd.hunks.len(), 1);
        let hunk = &fd.hunks[0];
        assert_eq!(hunk.old_start, 1);
        assert_eq!(hunk.old_count, 5);
        assert_eq!(hunk.new_start, 1);
        assert_eq!(hunk.new_count, 6);
        assert_eq!(hunk.lines.len(), 5);
        assert!(matches!(hunk.lines[0], DiffLine::Context(_)));
        assert!(matches!(hunk.lines[1], DiffLine::Removed(_)));
        assert!(matches!(hunk.lines[2], DiffLine::Added(_)));
        assert!(matches!(hunk.lines[3], DiffLine::Added(_)));
        assert!(matches!(hunk.lines[4], DiffLine::Context(_)));
    }

    #[test]
    fn test_binary_detection() {
        let fd = parse_diff("Binary files a/img.png and b/img.png differ\n");
        assert!(fd.is_binary);
        assert!(fd.hunks.is_empty());
    }

    #[test]
    fn test_binary_detection_git_binary_patch_format() {
        let diff =
            "diff --git a/img.png b/img.png\nindex abc..def 100644\nGIT binary patch\nliteral 123\nabc\n";
        let fd = parse_diff(diff);
        assert!(fd.is_binary);
        assert!(fd.hunks.is_empty());
    }

    #[test]
    fn test_patch_containing_binary_files_literal_is_not_binary() {
        let diff = r#"diff --git a/src/lib.rs b/src/lib.rs
index abc..def 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1 @@
-fn before() {}
+fn after() { println!("Binary files"); }
"#;

        let fd = parse_diff(diff);
        assert!(!fd.is_binary);
        assert_eq!(fd.hunks.len(), 1);
    }
}
