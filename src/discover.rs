use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::git;

/// A candidate benchmark or workload command discovered in a repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchCandidate {
    pub cmd: String,
    pub kind: String,
    pub source: String,
    pub confidence: String,
}

/// Zero-config repository discovery report returned by `oa discover` / `oa_discover`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverReport {
    pub repo_root: String,
    pub languages: Vec<String>,
    pub build_system: Option<String>,
    pub suggested_build_cmd: Option<String>,
    pub suggested_test_cmd: Option<String>,
    pub bench_candidates: Vec<BenchCandidate>,
    pub recommended_init_cmd: String,
    pub recommended_run_cmd: String,
    pub notes: Vec<String>,
}

fn push_unique(vec: &mut Vec<String>, val: &str) {
    if !vec.iter().any(|x| x == val) {
        vec.push(val.to_string());
    }
}

fn find_files_matching(
    root: &Path,
    subdir: &str,
    max_depth: usize,
    predicate: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let start = if subdir.is_empty() {
        root.to_path_buf()
    } else {
        root.join(subdir)
    };
    if !start.is_dir() {
        return Vec::new();
    }
    let mut results = Vec::new();
    let mut stack = vec![(start, 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.')
                || matches!(
                    name_str.as_ref(),
                    "target"
                        | "node_modules"
                        | "vendor"
                        | "build"
                        | "dist"
                        | "__pycache__"
                        | ".venv"
                        | "venv"
                        | "third_party"
                        | "external"
                )
            {
                continue;
            }
            if path.is_dir() {
                if depth < max_depth {
                    stack.push((path, depth + 1));
                }
            } else if path.is_file() && predicate(&path) {
                results.push(path);
                if results.len() >= 12 {
                    return results;
                }
            }
        }
    }
    results.sort();
    results
}

fn rel_str(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

fn detect_python_bin() -> &'static str {
    if cfg!(windows) {
        "python"
    } else {
        "python3"
    }
}

fn shell_quote_arg(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\\\""))
}

/// Scans a repository and infers languages, build/test commands, and benchmark candidates.
pub fn discover_repo(path: Option<&str>) -> Result<DiscoverReport, String> {
    let start = match path {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir().map_err(|e| format!("failed to get cwd: {e}"))?,
    };
    let root = git::repo_root(&start)
        .unwrap_or_else(|_| fs::canonicalize(&start).unwrap_or(start.clone()));

    let mut languages = Vec::new();
    let mut build_system: Option<String> = None;
    let mut suggested_build_cmd: Option<String> = None;
    let mut suggested_test_cmd: Option<String> = None;
    let mut bench_candidates: Vec<BenchCandidate> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    // 1. Rust (Cargo.toml)
    let cargo_toml = root.join("Cargo.toml");
    if cargo_toml.is_file() {
        push_unique(&mut languages, "rust");
        build_system = Some("cargo".to_string());
        suggested_build_cmd =
            Some("CARGO_PROFILE_RELEASE_DEBUG=true cargo build --release".to_string());
        suggested_test_cmd = Some("cargo test".to_string());

        let cargo_txt = fs::read_to_string(&cargo_toml).unwrap_or_default();
        let pkg_name = cargo_txt.lines().find_map(|line| {
            let t = line.trim();
            t.strip_prefix("name")
                .and_then(|r| r.trim_start().strip_prefix('='))
                .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string())
        });

        for bench_file in find_files_matching(&root, "benches", 1, |p| {
            p.extension().and_then(|e| e.to_str()) == Some("rs")
        }) {
            if let Some(stem) = bench_file.file_stem().and_then(|s| s.to_str()) {
                bench_candidates.push(BenchCandidate {
                    cmd: format!("cargo bench --bench {stem}"),
                    kind: "cargo-bench".to_string(),
                    source: rel_str(&root, &bench_file),
                    confidence: "high".to_string(),
                });
            }
        }
        if root.join("src/main.rs").is_file() {
            if let Some(name) = pkg_name {
                bench_candidates.push(BenchCandidate {
                    cmd: format!("./target/release/{name} --help"),
                    kind: "rust-binary".to_string(),
                    source: "src/main.rs".to_string(),
                    confidence: "low".to_string(),
                });
                notes.push(format!(
                    "Replace '--help' with a real workload input for ./target/release/{name} (>= 30 ms wall time)."
                ));
            }
        }
    }

    // 2. C / C++ (CMakeLists.txt or Makefile)
    let cmake_lists = root.join("CMakeLists.txt");
    if cmake_lists.is_file() {
        push_unique(&mut languages, "cpp");
        if build_system.is_none() {
            build_system = Some("cmake".to_string());
        }
        let build_dir = ["build", "out/build", "cmake-build-release"]
            .into_iter()
            .find(|d| root.join(d).is_dir());
        if let Some(bd) = build_dir {
            if suggested_build_cmd.is_none() {
                suggested_build_cmd = Some(format!("cmake --build {bd} --parallel"));
            }
            if suggested_test_cmd.is_none() {
                suggested_test_cmd = Some(format!("ctest --test-dir {bd} --output-on-failure"));
            }
        } else {
            if suggested_build_cmd.is_none() {
                suggested_build_cmd = Some(
                    "cmake -B build -DCMAKE_BUILD_TYPE=RelWithDebInfo && cmake --build build --parallel"
                        .to_string(),
                );
            }
            if suggested_test_cmd.is_none() {
                suggested_test_cmd = Some("ctest --test-dir build --output-on-failure".to_string());
            }
        }
        notes.push(
            "Build C/C++ with -DCMAKE_BUILD_TYPE=RelWithDebInfo so oa profile resolves ELF file:line hotspots and pahole DWARF struct padding."
                .to_string(),
        );
    } else if root.join("Makefile").is_file() && build_system.is_none() {
        build_system = Some("make".to_string());
        suggested_build_cmd = Some("make -j".to_string());
        suggested_test_cmd = Some("make test".to_string());
    }

    // 3. Go (go.mod)
    if root.join("go.mod").is_file() {
        push_unique(&mut languages, "go");
        if build_system.is_none() {
            build_system = Some("go".to_string());
        }
        if suggested_test_cmd.is_none() {
            suggested_test_cmd = Some("go test ./...".to_string());
        }
        let go_tests = find_files_matching(&root, "", 2, |p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with("_test.go"))
                .unwrap_or(false)
        });
        for tf in go_tests {
            if let Ok(txt) = fs::read_to_string(&tf) {
                if txt.contains("func Benchmark") {
                    bench_candidates.push(BenchCandidate {
                        cmd: "go test -bench=. -benchmem -run=^$ ./...".to_string(),
                        kind: "go-bench".to_string(),
                        source: rel_str(&root, &tf),
                        confidence: "high".to_string(),
                    });
                    break;
                }
            }
        }
    }

    // 4. Node.js / TypeScript (package.json)
    let pkg_json = root.join("package.json");
    if pkg_json.is_file() {
        let is_ts = root.join("tsconfig.json").is_file();
        push_unique(
            &mut languages,
            if is_ts { "typescript" } else { "javascript" },
        );
        if build_system.is_none() {
            build_system = Some("npm".to_string());
        }
        if let Ok(raw) = fs::read_to_string(&pkg_json) {
            if let Ok(val) = serde_json::from_str::<Value>(&raw) {
                if let Some(scripts) = val.get("scripts").and_then(|s| s.as_object()) {
                    if suggested_build_cmd.is_none() && scripts.contains_key("build") {
                        suggested_build_cmd = Some("npm run build".to_string());
                    }
                    if suggested_test_cmd.is_none() && scripts.contains_key("test") {
                        suggested_test_cmd = Some("npm test".to_string());
                    }
                    for key in ["bench", "benchmark", "perf"] {
                        if scripts.contains_key(key) {
                            bench_candidates.push(BenchCandidate {
                                cmd: format!("npm run {key}"),
                                kind: "npm-script".to_string(),
                                source: format!("package.json#scripts.{key}"),
                                confidence: "high".to_string(),
                            });
                        }
                    }
                }
            }
        }
        for js_bench in find_files_matching(&root, "", 2, |p| {
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            (name.ends_with(".js") || name.ends_with(".mjs"))
                && (name.contains("bench") || name.contains("perf"))
        }) {
            let rel = rel_str(&root, &js_bench);
            bench_candidates.push(BenchCandidate {
                cmd: format!("node {rel}"),
                kind: "node-script".to_string(),
                source: rel,
                confidence: "medium".to_string(),
            });
        }
    }

    // 5. Python (pyproject.toml, setup.py, pytest, or bench/tools scripts)
    let has_py_manifest = root.join("pyproject.toml").is_file()
        || root.join("setup.py").is_file()
        || root.join("pytest.ini").is_file()
        || root.join("requirements.txt").is_file();
    let py_benches = find_files_matching(&root, "", 2, |p| {
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        name.ends_with(".py") && (name.starts_with("bench") || name.contains("_bench"))
    });
    let py_tools = find_files_matching(&root, "tools", 1, |p| {
        p.extension().and_then(|e| e.to_str()) == Some("py")
    });

    if has_py_manifest || !py_benches.is_empty() || !py_tools.is_empty() {
        push_unique(&mut languages, "python");
        if build_system.is_none() && has_py_manifest {
            build_system = Some("python".to_string());
        }
        if suggested_test_cmd.is_none()
            && has_py_manifest
            && (root.join("tests").is_dir() || root.join("pytest.ini").is_file())
        {
            suggested_test_cmd = Some("pytest -q".to_string());
        }
        let py = detect_python_bin();
        for pb in py_benches {
            let rel = rel_str(&root, &pb);
            bench_candidates.push(BenchCandidate {
                cmd: format!("{py} {rel}"),
                kind: "python-bench".to_string(),
                source: rel,
                confidence: "high".to_string(),
            });
        }
        for pt in py_tools.into_iter().take(4) {
            let rel = rel_str(&root, &pt);
            bench_candidates.push(BenchCandidate {
                cmd: format!("{py} {rel}"),
                kind: "python-tool".to_string(),
                source: rel,
                confidence: "medium".to_string(),
            });
        }
    }

    // Sort candidates: high > medium > low.
    bench_candidates.sort_by_key(|c| match c.confidence.as_str() {
        "high" => 0,
        "medium" => 1,
        _ => 2,
    });

    let primary_bench = bench_candidates
        .first()
        .map(|c| c.cmd.clone())
        .unwrap_or_else(|| "<BENCH_CMD>".to_string());

    let mut init_parts = vec![
        "oa init".to_string(),
        format!("--bench {}", shell_quote_arg(&primary_bench)),
        "--metric pareto".to_string(),
    ];
    let mut run_parts = vec![
        "oa run".to_string(),
        format!("--bench {}", shell_quote_arg(&primary_bench)),
        "--metric pareto".to_string(),
    ];
    if let Some(ref b) = suggested_build_cmd {
        init_parts.push(format!("--build {}", shell_quote_arg(b)));
        run_parts.push(format!("--build {}", shell_quote_arg(b)));
    }
    if let Some(ref t) = suggested_test_cmd {
        init_parts.push(format!("--test {}", shell_quote_arg(t)));
        run_parts.push(format!("--test {}", shell_quote_arg(t)));
    }

    if bench_candidates.is_empty() {
        notes.push(
            "No benchmark script found automatically; pass a deterministic >= 30 ms workload via --bench."
                .to_string(),
        );
    }

    Ok(DiscoverReport {
        repo_root: root.to_string_lossy().to_string(),
        languages,
        build_system,
        suggested_build_cmd,
        suggested_test_cmd,
        bench_candidates,
        recommended_init_cmd: init_parts.join(" "),
        recommended_run_cmd: run_parts.join(" "),
        notes,
    })
}
