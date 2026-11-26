use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::git;
use crate::runner::{run_command, RunOutcome};
use crate::sha256::{sha256_file_hex, sha256_hex};

/// Locked SHA-256 digests of protected test, benchmark, and oracle files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileLocks {
    pub files: BTreeMap<String, String>,
}

/// Violation reported by Cheat Guard when a protected file was altered or removed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TamperViolation {
    pub path: String,
    pub reason: String,
}

/// Strategy used to verify behavioral equivalence between baseline and candidate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum EquivalencePolicy {
    #[default]
    Exact,
    Normalized {
        ignore_substrings: Vec<String>,
    },
    Oracle {
        command: String,
    },
}

/// Baseline output digest snapshot captured during `oa init`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputSnapshot {
    pub exit_code: i32,
    pub stdout_sha256: String,
    pub stdout_bytes: usize,
    pub stdout_preview: String,
    pub output_file_hashes: BTreeMap<String, String>,
}

/// Result of running equivalence verification on a candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquivalenceResult {
    pub passed: bool,
    pub exit_code_match: bool,
    pub stdout_match: bool,
    pub files_match: bool,
    pub oracle_passed: Option<bool>,
    pub candidate_stdout_sha256: String,
    pub details: String,
}

fn looks_like_protected_file(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split('/').collect();
    if parts.iter().any(|&p| {
        matches!(
            p,
            "test" | "tests" | "__tests__" | "bench" | "benches" | "benchmarks"
        )
    }) {
        return true;
    }
    let name = parts.last().copied().unwrap_or("");
    name.starts_with("test_")
        || name.ends_with("_test.py")
        || name.ends_with("_test.rs")
        || name.ends_with("_test.go")
        || name.ends_with(".test.js")
        || name.ends_with(".test.ts")
        || name.ends_with(".spec.js")
        || name.ends_with(".spec.ts")
        || name.starts_with("bench_")
}

/// Builds immutable SHA-256 locks for explicitly requested files plus auto-discovered test/bench files.
pub fn build_locks(
    root: &Path,
    explicit_paths: &[String],
    auto_lock_tests: bool,
) -> Result<FileLocks, String> {
    let mut files = BTreeMap::new();

    if auto_lock_tests {
        if let Ok(tracked) = git::list_tracked_files(root) {
            for rel in tracked {
                if looks_like_protected_file(&rel) {
                    let full = root.join(&rel);
                    if full.is_file() {
                        let hash = sha256_file_hex(&full)
                            .map_err(|e| format!("failed to hash protected file '{rel}': {e}"))?;
                        files.insert(rel, hash);
                    }
                }
            }
        }
    }

    for rel in explicit_paths {
        let normalized = rel.trim_start_matches("./").to_string();
        let full = root.join(&normalized);
        if full.is_file() {
            let hash = sha256_file_hex(&full)
                .map_err(|e| format!("failed to hash locked file '{normalized}': {e}"))?;
            files.insert(normalized, hash);
        } else {
            return Err(format!("locked file '{normalized}' does not exist"));
        }
    }

    Ok(FileLocks { files })
}

/// Checks every locked file against its initial SHA-256 digest.
pub fn check_locks(root: &Path, locks: &FileLocks) -> Vec<TamperViolation> {
    let mut violations = Vec::new();
    for (rel, expected) in &locks.files {
        let full = root.join(rel);
        match sha256_file_hex(&full) {
            Ok(actual) if actual == *expected => {}
            Ok(actual) => violations.push(TamperViolation {
                path: rel.clone(),
                reason: format!(
                    "modified protected file (expected {}.., got {}..)",
                    &expected[..12],
                    &actual[..12]
                ),
            }),
            Err(_) => violations.push(TamperViolation {
                path: rel.clone(),
                reason: "deleted or unreadable protected file".to_string(),
            }),
        }
    }
    violations
}

fn normalize_bytes(raw: &[u8], ignore_substrings: &[String]) -> Vec<u8> {
    if ignore_substrings.is_empty() {
        return raw.to_vec();
    }
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if ignore_substrings
            .iter()
            .any(|pat| line.contains(pat.as_str()))
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.into_bytes()
}

pub fn capture_snapshot(
    outcome: &RunOutcome,
    cwd: &Path,
    output_files: &[String],
    policy: &EquivalencePolicy,
) -> Result<OutputSnapshot, String> {
    let stdout_norm = match policy {
        EquivalencePolicy::Normalized { ignore_substrings } => {
            normalize_bytes(&outcome.stdout, ignore_substrings)
        }
        _ => outcome.stdout.clone(),
    };
    let stdout_sha256 = sha256_hex(&stdout_norm);
    let stdout_preview: String = String::from_utf8_lossy(&outcome.stdout)
        .chars()
        .take(400)
        .collect();

    let mut output_file_hashes = BTreeMap::new();
    for rel in output_files {
        let full = cwd.join(rel);
        let bytes = fs::read(&full)
            .map_err(|e| format!("failed to read declared output file '{rel}': {e}"))?;
        let norm = match policy {
            EquivalencePolicy::Normalized { ignore_substrings } => {
                normalize_bytes(&bytes, ignore_substrings)
            }
            _ => bytes,
        };
        output_file_hashes.insert(rel.clone(), sha256_hex(&norm));
    }

    Ok(OutputSnapshot {
        exit_code: outcome.exit_code,
        stdout_sha256,
        stdout_bytes: outcome.stdout.len(),
        stdout_preview,
        output_file_hashes,
    })
}

pub fn verify_equivalence(
    baseline: &OutputSnapshot,
    candidate_outcome: &RunOutcome,
    cwd: &Path,
    output_files: &[String],
    policy: &EquivalencePolicy,
) -> Result<EquivalenceResult, String> {
    let exit_code_match = candidate_outcome.exit_code == baseline.exit_code;

    if let EquivalencePolicy::Oracle { command } = policy {
        let oracle_out = run_command(command, cwd, None, false)?;
        let oracle_ok = oracle_out.exit_code == 0;
        let cand_sha = sha256_hex(&candidate_outcome.stdout);
        let details = if exit_code_match && oracle_ok {
            "oracle command exited 0 and exit code matched baseline".to_string()
        } else if !exit_code_match {
            format!(
                "exit code mismatch (baseline {}, candidate {})",
                baseline.exit_code, candidate_outcome.exit_code
            )
        } else {
            let err = String::from_utf8_lossy(&oracle_out.stderr);
            let out = String::from_utf8_lossy(&oracle_out.stdout);
            format!(
                "oracle failed (exit {}): {}{}",
                oracle_out.exit_code,
                out.chars().take(240).collect::<String>(),
                err.chars().take(240).collect::<String>()
            )
        };
        return Ok(EquivalenceResult {
            passed: exit_code_match && oracle_ok,
            exit_code_match,
            stdout_match: oracle_ok,
            files_match: oracle_ok,
            oracle_passed: Some(oracle_ok),
            candidate_stdout_sha256: cand_sha,
            details,
        });
    }

    let cand_snap = capture_snapshot(candidate_outcome, cwd, output_files, policy)?;
    let stdout_match = cand_snap.stdout_sha256 == baseline.stdout_sha256;
    let mut mismatched_files = Vec::new();
    for (rel, base_hash) in &baseline.output_file_hashes {
        match cand_snap.output_file_hashes.get(rel) {
            Some(h) if h == base_hash => {}
            Some(h) => mismatched_files.push(format!(
                "{rel} (expected {}.., got {}..)",
                &base_hash[..12],
                &h[..12]
            )),
            None => mismatched_files.push(format!("{rel} (missing)")),
        }
    }
    let files_match = mismatched_files.is_empty();
    let passed = exit_code_match && stdout_match && files_match;

    let details = if passed {
        format!(
            "exact output equivalence verified (exit {}, stdout sha256 {}.., {} output file(s))",
            cand_snap.exit_code,
            &cand_snap.stdout_sha256[..16],
            output_files.len()
        )
    } else if !exit_code_match {
        format!(
            "exit code mismatch (baseline {}, candidate {})",
            baseline.exit_code, cand_snap.exit_code
        )
    } else if !stdout_match {
        format!(
            "stdout SHA-256 diverged (baseline {}.. [{} bytes], candidate {}.. [{} bytes]); preview: {:?}",
            &baseline.stdout_sha256[..12],
            baseline.stdout_bytes,
            &cand_snap.stdout_sha256[..12],
            cand_snap.stdout_bytes,
            cand_snap.stdout_preview.chars().take(160).collect::<String>()
        )
    } else {
        format!("output file mismatch: {}", mismatched_files.join(", "))
    };

    Ok(EquivalenceResult {
        passed,
        exit_code_match,
        stdout_match,
        files_match,
        oracle_passed: None,
        candidate_stdout_sha256: cand_snap.stdout_sha256,
        details,
    })
}
