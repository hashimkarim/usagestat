//! Project names for local usage, shared by the daemon's reports and the CLI.
use std::path::{Path, PathBuf};

/// A working directory's project name. Agent worktrees are named after the
/// repository they belong to, not their generated folder:
/// `~/.t3/worktrees/<repo>/t3-…` and `<repo>/.claude/worktrees/agent-…`.
pub fn label(path: &str) -> String {
    let path = path.strip_prefix("file://").unwrap_or(path);
    // Collapse "." and ".." lexically; logs record the directory as typed.
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    if let Some(i) = parts.iter().rposition(|s| *s == "worktrees") {
        let before = i.checked_sub(1).map(|j| parts[j]);
        let generated = |s: &str| s.len() >= 6 && s.chars().all(|c| c.is_ascii_hexdigit());
        match (before, parts.get(i + 1), parts.get(i + 2)) {
            // <repo>/.claude/worktrees/<agent>: the repository owns the folder.
            (Some(".claude"), Some(_), _) if i >= 2 => return parts[i - 2].to_string(),
            // ~/.codex/worktrees/<id>/<repo>
            (_, Some(id), Some(repo)) if generated(id) => return repo.to_string(),
            // <tool>/worktrees/<repo>/<generated>
            (_, Some(repo), Some(_)) => return repo.to_string(),
            _ => {}
        }
    }
    parts
        .last()
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.to_string())
}

/// The project for a working directory, preferring the git checkout that
/// still exists on disk: a subdirectory counts toward its repository and a
/// linked worktree toward the main one. Gone or non-git paths use [`label`].
/// Results are memoised, since every log event repeats its directory.
pub fn resolve(path: &str) -> String {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(path) {
        return hit.clone();
    }
    let resolved = repository_root(Path::new(path.strip_prefix("file://").unwrap_or(path)))
        .map(|root| label(&root.to_string_lossy()))
        .unwrap_or_else(|| label(path));
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() < 4096 {
        cache.insert(path.to_owned(), resolved.clone());
    }
    resolved
}

/// The main checkout containing `dir`, if any. A home directory that is
/// itself a repository (dotfiles) is not treated as everyone's project.
fn repository_root(dir: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    for ancestor in dir.ancestors() {
        let git = ancestor.join(".git");
        let Ok(meta) = std::fs::symlink_metadata(&git) else {
            continue;
        };
        if home.as_deref() == Some(ancestor) {
            return None;
        }
        if meta.is_dir() {
            return Some(ancestor.to_path_buf());
        }
        // Linked worktree: "gitdir: <main>/.git/worktrees/<name>".
        let text = std::fs::read_to_string(&git).ok()?;
        let gitdir = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
        let gitdir = if gitdir.is_absolute() {
            gitdir
        } else {
            ancestor.join(gitdir)
        };
        return gitdir
            .ancestors()
            .find(|p| p.file_name().is_some_and(|n| n == ".git"))
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| Some(ancestor.to_path_buf()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::label as project_label;

    #[test]
    fn worktree_projects_are_named_after_their_repository() {
        for (path, label) in [
            ("/home/u/.t3/worktrees/htpc-stack/t3-7aa31368", "htpc-stack"),
            (
                "file:///home/u/.t3/worktrees/ai-workspace/t3-4e4b677a",
                "ai-workspace",
            ),
            (
                "/mnt/Git/htpc-stack/.claude/worktrees/agent-aad77b49",
                "htpc-stack",
            ),
            (
                "/home/u/.local/state/agenticdriver/worktrees/native-tools-sdk",
                "native-tools-sdk",
            ),
            (
                "/home/u/.t3/worktrees/usagestat/t3-1/crates/core",
                "usagestat",
            ),
            ("/mnt/shared/Git/usagestat", "usagestat"),
            ("file:///mnt/shared/Git/usagestat/", "usagestat"),
            ("/mnt/shared/Git/usagestat/crates/..", "usagestat"),
            ("/mnt/shared/Git/./usagestat", "usagestat"),
            ("C:\\Users\\u\\src\\app", "app"),
            (
                "/home/u/.codex/worktrees/1a2b3c4d/punch-website",
                "punch-website",
            ),
            (
                "/home/u/.codex/worktrees/1a2b3c4d/punch-website/src",
                "punch-website",
            ),
        ] {
            assert_eq!(project_label(path), label, "{path}");
        }
    }

    #[test]
    fn live_checkouts_resolve_to_their_main_repository() {
        let dir = crate::storage::temporary_directory().unwrap();
        let repo = dir.path().join("my-app");
        std::fs::create_dir_all(repo.join(".git/worktrees/feature")).unwrap();
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        let tree = dir.path().join("my-app-wt-ba1715");
        std::fs::create_dir_all(tree.join("crates")).unwrap();
        std::fs::write(
            tree.join(".git"),
            format!(
                "gitdir: {}\n",
                repo.join(".git/worktrees/feature").display()
            ),
        )
        .unwrap();
        for (path, name) in [
            (repo.clone(), "my-app"),
            (repo.join("src/deep"), "my-app"),
            (tree.join("crates"), "my-app"),
            (dir.path().join("gone/t3-1234"), "t3-1234"),
        ] {
            assert_eq!(super::resolve(&path.to_string_lossy()), name, "{path:?}");
        }
    }
}
