use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::sha256::Sha256;

static INDEX_SEQ: AtomicU64 = AtomicU64::new(0);

fn git_cmd(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("failed to run git {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git {} failed: {stderr}", args.join(" ")));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn repo_root(dir: &Path) -> Result<PathBuf, String> {
    let out = git_cmd(dir, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(out))
}

pub fn head_sha(root: &Path) -> Result<String, String> {
    git_cmd(root, &["rev-parse", "HEAD"])
}

pub fn resolve_rev(root: &Path, rev: &str) -> Result<String, String> {
    git_cmd(root, &["rev-parse", "--verify", rev])
}

pub fn has_uncommitted_tracked_changes(root: &Path) -> Result<bool, String> {
    let out = git_cmd(root, &["status", "--porcelain", "--untracked-files=no"])?;
    Ok(!out.is_empty())
}

fn is_ignored_artifact(rel: &str) -> bool {
    rel.starts_with(".oa/")
        || rel.starts_with(".oa\\")
        || rel == ".oa"
        || rel.ends_with(".pyc")
        || rel.ends_with(".pyo")
        || rel.split(['/', '\\']).any(|seg| seg == "__pycache__")
}

pub fn changed_files_vs_ref(root: &Path, git_ref: &str) -> Result<Vec<String>, String> {
    let out = git_cmd(root, &["diff", "--name-only", git_ref])?;
    let mut files: Vec<String> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !is_ignored_artifact(l))
        .map(str::to_string)
        .collect();
    let untracked = git_cmd(
        root,
        &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "--",
            ".",
            ":!.oa",
        ],
    )?;
    for line in untracked.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !is_ignored_artifact(trimmed) {
            files.push(trimmed.to_string());
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// Computes a deterministic SHA-256 fingerprint of the current working tree (excluding `.oa/`).
pub fn working_tree_fingerprint(root: &Path) -> Result<String, String> {
    let head = head_sha(root)?;
    let diff = git_cmd(root, &["diff", "HEAD", "--", ".", ":!.oa"])?;
    let untracked = git_cmd(
        root,
        &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "--",
            ".",
            ":!.oa",
        ],
    )?;

    let mut hasher = Sha256::new();
    hasher.update(head.as_bytes());
    hasher.update(b"\n--DIFF--\n");
    hasher.update(diff.as_bytes());
    hasher.update(b"\n--UNTRACKED--\n");

    let mut untracked_list: Vec<&str> = untracked
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !is_ignored_artifact(l))
        .collect();
    untracked_list.sort();
    for rel in untracked_list {
        hasher.update(rel.as_bytes());
        hasher.update(b"\0");
        if let Ok(bytes) = fs::read(root.join(rel)) {
            hasher.update(&bytes);
        }
        hasher.update(b"\n");
    }
    Ok(hasher.finalize_hex())
}

/// Creates a snapshot commit object of the current working tree (excluding `.oa/`) without moving HEAD.
pub fn snapshot_working_tree(
    root: &Path,
    parent_ref: &str,
    target_ref: &str,
    message: &str,
) -> Result<String, String> {
    let oa_dir = root.join(".oa");
    fs::create_dir_all(&oa_dir).map_err(|e| format!("failed to create .oa dir: {e}"))?;
    let seq = INDEX_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_index = oa_dir.join(format!("tmp_index_{}_{}", std::process::id(), seq));
    let _ = fs::remove_file(&tmp_index);

    let parent_sha = git_cmd(root, &["rev-parse", parent_ref])?;

    let read_tree = Command::new("git")
        .args(["read-tree", &parent_sha])
        .current_dir(root)
        .env("GIT_INDEX_FILE", &tmp_index)
        .output()
        .map_err(|e| format!("git read-tree failed: {e}"))?;
    if !read_tree.status.success() {
        let _ = fs::remove_file(&tmp_index);
        return Err(format!(
            "git read-tree failed: {}",
            String::from_utf8_lossy(&read_tree.stderr)
        ));
    }

    let add = Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .env("GIT_INDEX_FILE", &tmp_index)
        .output()
        .map_err(|e| format!("git add failed: {e}"))?;
    if !add.status.success() {
        let _ = fs::remove_file(&tmp_index);
        return Err(format!(
            "git add failed: {}",
            String::from_utf8_lossy(&add.stderr)
        ));
    }

    let _ = Command::new("git")
        .args(["rm", "-rf", "--cached", "--ignore-unmatch", ".oa"])
        .current_dir(root)
        .env("GIT_INDEX_FILE", &tmp_index)
        .output();

    if let Ok(staged) = Command::new("git")
        .args(["ls-files"])
        .current_dir(root)
        .env("GIT_INDEX_FILE", &tmp_index)
        .output()
    {
        if staged.status.success() {
            let ignored: Vec<String> = String::from_utf8_lossy(&staged.stdout)
                .lines()
                .map(str::trim)
                .filter(|p| !p.is_empty() && is_ignored_artifact(p))
                .map(str::to_string)
                .collect();
            if !ignored.is_empty() {
                let mut rm_cmd = Command::new("git");
                rm_cmd
                    .args(["rm", "-rf", "--cached", "--ignore-unmatch", "--"])
                    .args(&ignored)
                    .current_dir(root)
                    .env("GIT_INDEX_FILE", &tmp_index);
                let _ = rm_cmd.output();
            }
        }
    }

    let write_tree = Command::new("git")
        .args(["write-tree"])
        .current_dir(root)
        .env("GIT_INDEX_FILE", &tmp_index)
        .output()
        .map_err(|e| format!("git write-tree failed: {e}"))?;
    let _ = fs::remove_file(&tmp_index);
    if !write_tree.status.success() {
        return Err(format!(
            "git write-tree failed: {}",
            String::from_utf8_lossy(&write_tree.stderr)
        ));
    }
    let tree_sha = String::from_utf8_lossy(&write_tree.stdout)
        .trim()
        .to_string();

    let commit_out = Command::new("git")
        .args(["commit-tree", &tree_sha, "-p", &parent_sha, "-m", message])
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "oa")
        .env("GIT_AUTHOR_EMAIL", "oa@localhost")
        .env("GIT_COMMITTER_NAME", "oa")
        .env("GIT_COMMITTER_EMAIL", "oa@localhost")
        .output()
        .map_err(|e| format!("git commit-tree failed: {e}"))?;
    if !commit_out.status.success() {
        return Err(format!(
            "git commit-tree failed: {}",
            String::from_utf8_lossy(&commit_out.stderr)
        ));
    }
    let commit_sha = String::from_utf8_lossy(&commit_out.stdout)
        .trim()
        .to_string();

    git_cmd(root, &["update-ref", target_ref, &commit_sha])?;
    Ok(commit_sha)
}

/// Ensures a detached shadow worktree exists at `worktree_path` checked out to `commit_sha`.
pub fn ensure_shadow_worktree(
    root: &Path,
    worktree_path: &Path,
    commit_sha: &str,
) -> Result<(), String> {
    let _ = git_cmd(root, &["worktree", "prune"]);
    if worktree_path.exists() {
        let checkout = Command::new("git")
            .args(["checkout", "--detach", "--force", commit_sha])
            .current_dir(worktree_path)
            .output();
        if matches!(checkout, Ok(o) if o.status.success()) {
            let _ = git_cmd(worktree_path, &["clean", "-fd"]);
            return Ok(());
        }
        let _ = git_cmd(
            root,
            &[
                "worktree",
                "remove",
                "--force",
                &worktree_path.to_string_lossy(),
            ],
        );
        let _ = fs::remove_dir_all(worktree_path);
        let _ = git_cmd(root, &["worktree", "prune"]);
    }

    if let Some(parent) = worktree_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("failed to create worktree parent: {e}"))?;
    }
    git_cmd(
        root,
        &[
            "worktree",
            "add",
            "--detach",
            "--force",
            &worktree_path.to_string_lossy(),
            commit_sha,
        ],
    )?;
    Ok(())
}

/// Restores the main working tree (excluding `.oa/`) to match `target_ref`.
pub fn restore_working_tree_to_ref(root: &Path, target_ref: &str) -> Result<(), String> {
    let sha = git_cmd(root, &["rev-parse", target_ref])?;
    git_cmd(root, &["checkout", &sha, "--", "."])?;

    let target_files: std::collections::BTreeSet<String> =
        git_cmd(root, &["ls-tree", "-r", "--name-only", &sha])?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();

    let untracked = git_cmd(
        root,
        &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "--",
            ".",
            ":!.oa",
        ],
    )?;
    for rel in untracked.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !target_files.contains(rel) {
            let _ = fs::remove_file(root.join(rel));
        }
    }
    Ok(())
}

pub fn update_ref(root: &Path, ref_name: &str, commit_sha: &str) -> Result<(), String> {
    git_cmd(root, &["update-ref", ref_name, commit_sha])?;
    Ok(())
}

pub fn list_tracked_files(root: &Path) -> Result<Vec<String>, String> {
    let out = git_cmd(root, &["ls-files"])?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

pub fn diff_stat_refs(root: &Path, from_ref: &str, to_ref: &str) -> String {
    git_cmd(root, &["diff", "--stat", from_ref, to_ref]).unwrap_or_default()
}

pub fn diff_refs(root: &Path, from_ref: &str, to_ref: &str) -> Result<String, String> {
    let output = Command::new("git")
        .args(["diff", from_ref, to_ref])
        .current_dir(root)
        .output()
        .map_err(|e| format!("failed to run git diff {from_ref} {to_ref}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git diff {from_ref} {to_ref} failed: {stderr}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn diff_name_only_refs(root: &Path, from_ref: &str, to_ref: &str) -> Vec<String> {
    git_cmd(root, &["diff", "--name-only", from_ref, to_ref])
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !is_ignored_artifact(l))
        .map(str::to_string)
        .collect()
}

pub fn remove_shadow_worktree(root: &Path, worktree_path: &Path) -> bool {
    if !worktree_path.exists() {
        return false;
    }
    let _ = git_cmd(
        root,
        &[
            "worktree",
            "remove",
            "--force",
            &worktree_path.to_string_lossy(),
        ],
    );
    if worktree_path.exists() {
        let _ = fs::remove_dir_all(worktree_path);
    }
    let _ = git_cmd(root, &["worktree", "prune"]);
    true
}

pub fn delete_oa_refs(root: &Path) -> Result<Vec<String>, String> {
    let raw = git_cmd(root, &["for-each-ref", "--format=%(refname)", "refs/oa/"])?;
    let mut deleted = Vec::new();
    for r in raw.lines().map(str::trim).filter(|l| !l.is_empty()) {
        git_cmd(root, &["update-ref", "-d", r])?;
        deleted.push(r.to_string());
    }
    Ok(deleted)
}

/// Ensures `.oa/` is excluded via `.git/info/exclude` without dirtying tracked `.gitignore`.
pub fn ensure_oa_git_excluded(root: &Path) -> Result<(), String> {
    let rel = git_cmd(root, &["rev-parse", "--git-path", "info/exclude"])?;
    let exclude_path = if Path::new(&rel).is_absolute() {
        PathBuf::from(rel)
    } else {
        root.join(rel)
    };
    if let Some(parent) = exclude_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let existing = fs::read_to_string(&exclude_path).unwrap_or_default();
    if existing
        .lines()
        .any(|line| line.trim() == ".oa/" || line.trim() == ".oa")
    {
        return Ok(());
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(".oa/\n");
    fs::write(&exclude_path, updated)
        .map_err(|e| format!("failed to update {}: {e}", exclude_path.display()))?;
    Ok(())
}
