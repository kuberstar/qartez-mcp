use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct DetectedToolchain {
    pub name: String,
    pub build_tool: String,
    pub test_cmd: Vec<String>,
    pub build_cmd: Vec<String>,
    pub lint_cmd: Option<Vec<String>>,
    pub typecheck_cmd: Option<Vec<String>>,
    /// Relative subdirectory that owns this toolchain. `None` means the
    /// project root. Set by `detect_subdir_toolchains` so monorepo
    /// reports can tell the caller "Cargo.toml under qartez-public/"
    /// instead of silently reporting no build command.
    pub subdir: Option<String>,
}

/// (test_cmd, build_cmd, lint_cmd, typecheck_cmd)
type NodeScripts = (
    Vec<String>,
    Vec<String>,
    Option<Vec<String>>,
    Option<Vec<String>>,
);

/// Returns the first detected toolchain (backward-compatible entry point).
pub fn detect_toolchain(project_root: &Path) -> Option<DetectedToolchain> {
    detect_all_toolchains(project_root).into_iter().next()
}

/// Detects all toolchains present in the project root. A polyglot repo
/// (e.g. Rust + Node) returns one entry per toolchain in priority order.
pub fn detect_all_toolchains(project_root: &Path) -> Vec<DetectedToolchain> {
    let mut toolchains = Vec::new();

    if project_root.join("Cargo.toml").exists() {
        toolchains.push(DetectedToolchain {
            name: "rust".to_string(),
            build_tool: "cargo".to_string(),
            test_cmd: vec!["cargo".into(), "test".into()],
            build_cmd: vec!["cargo".into(), "build".into()],
            lint_cmd: Some(vec!["cargo".into(), "clippy".into()]),
            typecheck_cmd: Some(vec!["cargo".into(), "check".into()]),
            subdir: None,
        });
    }

    if project_root.join("go.mod").exists() {
        toolchains.push(DetectedToolchain {
            name: "go".to_string(),
            build_tool: "go".to_string(),
            test_cmd: vec!["go".into(), "test".into(), "./...".into()],
            build_cmd: vec!["go".into(), "build".into(), "./...".into()],
            lint_cmd: Some(vec!["golangci-lint".into(), "run".into()]),
            typecheck_cmd: Some(vec!["go".into(), "vet".into(), "./...".into()]),
            subdir: None,
        });
    }

    if project_root.join("package.json").exists() {
        let build_tool = detect_node_package_manager(project_root);
        let (test_cmd, build_cmd, lint_cmd, typecheck_cmd) =
            detect_node_scripts(project_root, &build_tool);

        toolchains.push(DetectedToolchain {
            name: "node".to_string(),
            build_tool,
            test_cmd,
            build_cmd,
            lint_cmd,
            typecheck_cmd,
            subdir: None,
        });
    }

    if project_root.join("pyproject.toml").exists() || project_root.join("setup.py").exists() {
        toolchains.push(DetectedToolchain {
            name: "python".to_string(),
            build_tool: "pip".to_string(),
            test_cmd: vec!["pytest".into()],
            build_cmd: vec!["python".into(), "-m".into(), "build".into()],
            lint_cmd: Some(vec!["ruff".into(), "check".into(), ".".into()]),
            typecheck_cmd: Some(vec!["mypy".into(), ".".into()]),
            subdir: None,
        });
    }

    if project_root.join("pom.xml").exists() {
        toolchains.push(DetectedToolchain {
            name: "java".to_string(),
            build_tool: "maven".to_string(),
            test_cmd: vec!["mvn".into(), "test".into()],
            build_cmd: vec!["mvn".into(), "package".into()],
            lint_cmd: None,
            typecheck_cmd: Some(vec!["mvn".into(), "compile".into()]),
            subdir: None,
        });
    }

    if project_root.join("build.gradle").exists()
        || project_root.join("build.gradle.kts").exists()
        || project_root.join("settings.gradle").exists()
        || project_root.join("settings.gradle.kts").exists()
    {
        toolchains.push(DetectedToolchain {
            name: "java".to_string(),
            build_tool: "gradle".to_string(),
            test_cmd: vec!["./gradlew".into(), "test".into()],
            build_cmd: vec!["./gradlew".into(), "build".into()],
            lint_cmd: None,
            typecheck_cmd: Some(vec!["./gradlew".into(), "compileJava".into()]),
            subdir: None,
        });
    }

    if project_root.join("build.sbt").exists() {
        toolchains.push(DetectedToolchain {
            name: "scala".to_string(),
            build_tool: "sbt".to_string(),
            test_cmd: vec!["sbt".into(), "test".into()],
            build_cmd: vec!["sbt".into(), "compile".into()],
            lint_cmd: None,
            typecheck_cmd: None,
            subdir: None,
        });
    }

    if project_root.join("Gemfile").exists() {
        toolchains.push(DetectedToolchain {
            name: "ruby".to_string(),
            build_tool: "bundle".to_string(),
            test_cmd: vec!["bundle".into(), "exec".into(), "rake".into(), "test".into()],
            build_cmd: vec![
                "bundle".into(),
                "exec".into(),
                "rake".into(),
                "build".into(),
            ],
            lint_cmd: Some(vec!["bundle".into(), "exec".into(), "rubocop".into()]),
            typecheck_cmd: None,
            subdir: None,
        });
    }

    if project_root.join("pubspec.yaml").exists() {
        let pubspec =
            std::fs::read_to_string(project_root.join("pubspec.yaml")).unwrap_or_default();
        let is_flutter = pubspec_uses_flutter(&pubspec);
        let has_build_runner = pubspec.contains("build_runner:");
        let driver = if is_flutter { "flutter" } else { "dart" };

        let build_cmd = if has_build_runner {
            vec![
                "dart".into(),
                "run".into(),
                "build_runner".into(),
                "build".into(),
                "--delete-conflicting-outputs".into(),
            ]
        } else {
            vec![driver.into(), "pub".into(), "get".into()]
        };

        toolchains.push(DetectedToolchain {
            name: if is_flutter {
                "flutter".into()
            } else {
                "dart".into()
            },
            build_tool: driver.into(),
            test_cmd: vec![driver.into(), "test".into()],
            build_cmd,
            lint_cmd: Some(vec![driver.into(), "analyze".into()]),
            // `dart analyze` covers static type checking - no separate typechecker.
            typecheck_cmd: Some(vec![driver.into(), "analyze".into()]),
            subdir: None,
        });
    }

    if project_root.join("Makefile").exists() {
        toolchains.push(DetectedToolchain {
            name: "make".to_string(),
            build_tool: "make".to_string(),
            test_cmd: vec!["make".into(), "test".into()],
            build_cmd: vec!["make".into(), "build".into()],
            lint_cmd: Some(vec!["make".into(), "lint".into()]),
            typecheck_cmd: None,
            subdir: None,
        });
    }

    toolchains
}

/// Walk the first-level subdirectories of `project_root` looking for
/// project manifests (`Cargo.toml`, `package.json`, `go.mod`, etc.) and
/// return a `DetectedToolchain` for each hit, tagged with the subdir
/// path so the caller can distinguish "root Cargo" from "workspace
/// member Cargo". Only one level deep; nested monorepos (e.g. Nx /
/// turborepo trees) need a dedicated recursive detector.
///
/// Skips hidden directories and the conventional VCS/build ignore set
/// (`target/`, `node_modules/`, `.git/`, etc.) so a large build cache
/// cannot burn IO budget.
pub fn detect_subdir_toolchains(project_root: &Path, max_subdirs: usize) -> Vec<DetectedToolchain> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(project_root) else {
        return out;
    };
    let mut subdirs: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().ok().is_some_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| {
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                return false;
            };
            if name.starts_with('.') {
                return false;
            }
            !matches!(
                name,
                "target"
                    | "node_modules"
                    | "dist"
                    | "build"
                    | "__pycache__"
                    | "venv"
                    | ".venv"
                    | "vendor"
                    | ".cache"
            )
        })
        .collect();
    subdirs.sort();
    subdirs.truncate(max_subdirs);

    for subdir in &subdirs {
        let rel = subdir
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string());
        let Some(rel) = rel else { continue };
        for mut tc in detect_all_toolchains(subdir) {
            tc.subdir = Some(rel.clone());
            out.push(tc);
        }
    }
    out
}

/// Checks whether a binary is available on PATH using pure Rust PATH search.
/// Works on both Unix and Windows without shelling out to `which`.
pub fn binary_available(name: &str) -> bool {
    // Skip path-relative binaries like ./gradlew - they are project-local.
    if name.starts_with('.') || name.contains('/') {
        return true;
    }
    let path_var = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return true;
        }
        // On Windows, also try with common executable extensions
        if cfg!(windows) {
            for ext in &["exe", "cmd", "bat", "com"] {
                let with_ext = dir.join(format!("{name}.{ext}"));
                if with_ext.is_file() {
                    return true;
                }
            }
        }
    }
    false
}

fn pubspec_uses_flutter(pubspec: &str) -> bool {
    // Flutter projects declare `flutter:` under *runtime* `dependencies:` with
    // `sdk: flutter`, or include a top-level `flutter:` configuration block.
    // `dev_dependencies:` is explicitly excluded - pure-Dart packages commonly
    // test with `flutter_test`, and we must not flip those to the Flutter
    // toolchain.
    let mut in_runtime_deps = false;
    let mut saw_flutter_key = false;
    for raw in pubspec.lines() {
        let line = raw.split('#').next().unwrap_or("");
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        let indent = trimmed.len() - trimmed.trim_start().len();
        let body = trimmed.trim_start();

        if indent == 0 {
            in_runtime_deps = body == "dependencies:";
            if body == "flutter:" {
                return true;
            }
            saw_flutter_key = false;
            continue;
        }

        if in_runtime_deps && indent == 2 && body == "flutter:" {
            saw_flutter_key = true;
            continue;
        }
        if saw_flutter_key && indent >= 4 && body.starts_with("sdk:") && body.contains("flutter") {
            return true;
        }
        if indent <= 2 {
            saw_flutter_key = false;
        }
    }
    false
}

fn detect_node_package_manager(project_root: &Path) -> String {
    if project_root.join("bun.lockb").exists() || project_root.join("bun.lock").exists() {
        "bun".to_string()
    } else if project_root.join("yarn.lock").exists() {
        "yarn".to_string()
    } else if project_root.join("pnpm-lock.yaml").exists() {
        "pnpm".to_string()
    } else {
        "npm".to_string()
    }
}

fn detect_node_scripts(project_root: &Path, build_tool: &str) -> NodeScripts {
    let pkg_json_path = project_root.join("package.json");
    let scripts = read_package_json_scripts(&pkg_json_path);

    let run = |script: &str| -> Vec<String> {
        if build_tool == "npm" {
            vec!["npm".into(), "run".into(), script.into()]
        } else {
            vec![build_tool.into(), "run".into(), script.into()]
        }
    };

    let test_cmd = if scripts.contains(&"test".to_string()) {
        run("test")
    } else {
        vec![build_tool.into(), "test".into()]
    };

    let build_cmd = if scripts.contains(&"build".to_string()) {
        run("build")
    } else {
        vec![build_tool.into(), "run".into(), "build".into()]
    };

    let lint_cmd = if scripts.contains(&"lint".to_string()) {
        Some(run("lint"))
    } else {
        None
    };

    let typecheck_cmd = if scripts.contains(&"typecheck".to_string()) {
        Some(run("typecheck"))
    } else if scripts.contains(&"type-check".to_string()) {
        Some(run("type-check"))
    } else if project_root.join("tsconfig.json").exists() {
        Some(vec!["npx".into(), "tsc".into(), "--noEmit".into()])
    } else {
        None
    };

    (test_cmd, build_cmd, lint_cmd, typecheck_cmd)
}

fn read_package_json_scripts(path: &Path) -> Vec<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    parsed
        .get("scripts")
        .and_then(|s| s.as_object())
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default()
}

/// Executes `cmd[0]` with `cmd[1..]` as arguments, plus an optional `filter`.
///
/// `cmd` is trusted: callers build it from toolchain detection
/// (`detect_toolchain`, `package.json` scripts, hardcoded build-tool names).
/// MCP parameters never flow into `cmd[0]`. The `filter` path is the only
/// caller-controlled slot and is validated in `project.rs` to reject values
/// starting with `-`.
pub fn run_command(
    project_root: &Path,
    cmd: &[String],
    filter: Option<&str>,
    timeout_secs: u32,
) -> std::result::Result<(i32, String), String> {
    if cmd.is_empty() {
        return Err("Empty command".to_string());
    }

    let mut args: Vec<&str> = cmd[1..].iter().map(|s| s.as_str()).collect();

    if let Some(f) = filter {
        args.push(f);
    }

    let mut child = Command::new(&cmd[0])
        .args(&args)
        .current_dir(project_root)
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run '{}': {}", cmd[0], e))?;

    let child_stdout = child.stdout.take();
    let child_stderr = child.stderr.take();

    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut r) = child_stdout {
            let _ = std::io::Read::read_to_end(&mut r, &mut buf);
        }
        buf
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut r) = child_stderr {
            let _ = std::io::Read::read_to_end(&mut r, &mut buf);
        }
        buf
    });

    let timeout = Duration::from_secs(timeout_secs as u64);
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "Command '{}' timed out after {}s",
                        cmd[0], timeout_secs
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("Error waiting for '{}': {}", cmd[0], e)),
        }
    }

    let exit_code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
    let stdout_bytes = stdout_handle.join().unwrap_or_default();
    let stderr_bytes = stderr_handle.join().unwrap_or_default();
    let stdout = String::from_utf8_lossy(&stdout_bytes);
    let stderr = String::from_utf8_lossy(&stderr_bytes);

    let mut combined = String::new();
    if !stdout.is_empty() {
        combined.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }

    const MAX_OUTPUT: usize = 4000;
    if combined.len() > MAX_OUTPUT {
        let mut end = MAX_OUTPUT;
        while !combined.is_char_boundary(end) {
            end -= 1;
        }
        combined.truncate(end);
        combined.push_str("\n... (output truncated)");
    }

    Ok((exit_code, combined))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_detect_rust_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();

        let tc = detect_toolchain(dir.path()).unwrap();
        assert_eq!(tc.name, "rust");
        assert_eq!(tc.build_tool, "cargo");
        assert_eq!(tc.test_cmd, vec!["cargo", "test"]);
        assert_eq!(tc.build_cmd, vec!["cargo", "build"]);
    }

    #[test]
    fn test_detect_go_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("go.mod"), "module example.com/test").unwrap();

        let tc = detect_toolchain(dir.path()).unwrap();
        assert_eq!(tc.name, "go");
        assert_eq!(tc.build_tool, "go");
    }

    #[test]
    fn test_detect_node_npm() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"jest","build":"tsc"}}"#,
        )
        .unwrap();

        let tc = detect_toolchain(dir.path()).unwrap();
        assert_eq!(tc.name, "node");
        assert_eq!(tc.build_tool, "npm");
    }

    #[test]
    fn test_detect_node_bun() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join("bun.lockb"), "").unwrap();

        let tc = detect_toolchain(dir.path()).unwrap();
        assert_eq!(tc.name, "node");
        assert_eq!(tc.build_tool, "bun");
    }

    #[test]
    fn test_detect_python_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "[project]").unwrap();

        let tc = detect_toolchain(dir.path()).unwrap();
        assert_eq!(tc.name, "python");
    }

    #[test]
    fn test_detect_maven_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pom.xml"), "<project></project>").unwrap();

        let tc = detect_toolchain(dir.path()).unwrap();
        assert_eq!(tc.name, "java");
        assert_eq!(tc.build_tool, "maven");
        assert_eq!(tc.test_cmd, vec!["mvn", "test"]);
    }

    #[test]
    fn test_detect_gradle_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("build.gradle"), "").unwrap();

        let tc = detect_toolchain(dir.path()).unwrap();
        assert_eq!(tc.name, "java");
        assert_eq!(tc.build_tool, "gradle");
    }

    #[test]
    fn test_detect_gradle_kts_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("build.gradle.kts"), "").unwrap();

        let tc = detect_toolchain(dir.path()).unwrap();
        assert_eq!(tc.name, "java");
        assert_eq!(tc.build_tool, "gradle");
    }

    #[test]
    fn test_detect_sbt_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("build.sbt"), "").unwrap();

        let tc = detect_toolchain(dir.path()).unwrap();
        assert_eq!(tc.name, "scala");
    }

    #[test]
    fn test_detect_dart_library() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pubspec.yaml"),
            "name: my_lib\nenvironment:\n  sdk: '>=3.0.0 <4.0.0'\n",
        )
        .unwrap();

        let tc = detect_toolchain(dir.path()).unwrap();
        assert_eq!(tc.name, "dart");
        assert_eq!(tc.build_tool, "dart");
        assert_eq!(tc.test_cmd, vec!["dart", "test"]);
        assert_eq!(tc.build_cmd, vec!["dart", "pub", "get"]);
        assert_eq!(tc.lint_cmd.unwrap(), vec!["dart", "analyze"]);
        assert_eq!(tc.typecheck_cmd.unwrap(), vec!["dart", "analyze"]);
    }

    #[test]
    fn test_detect_dart_with_build_runner() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pubspec.yaml"),
            "name: my_lib\ndev_dependencies:\n  build_runner: ^2.4.0\n",
        )
        .unwrap();

        let tc = detect_toolchain(dir.path()).unwrap();
        assert_eq!(tc.name, "dart");
        assert_eq!(
            tc.build_cmd,
            vec![
                "dart",
                "run",
                "build_runner",
                "build",
                "--delete-conflicting-outputs",
            ]
        );
    }

    #[test]
    fn test_detect_flutter_via_dependency() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pubspec.yaml"),
            "name: my_app\ndependencies:\n  flutter:\n    sdk: flutter\n",
        )
        .unwrap();

        let tc = detect_toolchain(dir.path()).unwrap();
        assert_eq!(tc.name, "flutter");
        assert_eq!(tc.build_tool, "flutter");
        assert_eq!(tc.test_cmd, vec!["flutter", "test"]);
        assert_eq!(tc.build_cmd, vec!["flutter", "pub", "get"]);
        assert_eq!(tc.lint_cmd.unwrap(), vec!["flutter", "analyze"]);
    }

    #[test]
    fn test_flutter_sdk_under_dev_dependencies_stays_dart() {
        // Pure-Dart libraries commonly depend on `flutter_test` for their
        // widget tests, which pulls `flutter: sdk: flutter` into
        // `dev_dependencies`. The toolchain must stay `dart`, not flip to
        // `flutter`.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pubspec.yaml"),
            "name: pure_dart_lib\n\
             dev_dependencies:\n\
             \u{0020}\u{0020}flutter:\n\
             \u{0020}\u{0020}\u{0020}\u{0020}sdk: flutter\n\
             \u{0020}\u{0020}flutter_test:\n\
             \u{0020}\u{0020}\u{0020}\u{0020}sdk: flutter\n",
        )
        .unwrap();

        let tc = detect_toolchain(dir.path()).unwrap();
        assert_eq!(tc.name, "dart", "dev-only flutter must stay dart toolchain");
    }

    #[test]
    fn test_detect_flutter_via_top_level_block() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pubspec.yaml"),
            "name: my_app\nflutter:\n  uses-material-design: true\n",
        )
        .unwrap();

        let tc = detect_toolchain(dir.path()).unwrap();
        assert_eq!(tc.name, "flutter");
    }

    #[test]
    fn test_detect_no_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        let tc = detect_toolchain(dir.path());
        assert!(tc.is_none());
    }

    #[test]
    fn test_detect_all_polyglot() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();

        let all = detect_all_toolchains(dir.path());
        assert_eq!(all.len(), 2, "should detect both Rust and Node");
        assert_eq!(all[0].name, "rust");
        assert_eq!(all[1].name, "node");
    }

    #[test]
    fn test_detect_all_empty() {
        let dir = tempfile::tempdir().unwrap();
        let all = detect_all_toolchains(dir.path());
        assert!(all.is_empty());
    }

    #[test]
    fn test_binary_available_known() {
        assert!(binary_available("echo"), "echo should be on PATH");
    }

    #[test]
    fn test_binary_available_missing() {
        assert!(
            !binary_available("nonexistent_binary_xyz_42"),
            "fake binary should not be on PATH"
        );
    }

    #[test]
    fn test_binary_available_relative_path() {
        assert!(
            binary_available("./gradlew"),
            "path-relative binaries should be assumed available"
        );
    }

    #[test]
    fn test_run_command_echo() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = vec!["echo".to_string(), "hello".to_string()];
        let (code, output) = run_command(dir.path(), &cmd, None, 10).unwrap();
        assert_eq!(code, 0);
        assert!(output.contains("hello"));
    }

    #[test]
    fn test_run_command_with_filter() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = vec!["echo".to_string()];
        let (code, output) = run_command(dir.path(), &cmd, Some("world"), 10).unwrap();
        assert_eq!(code, 0);
        assert!(output.contains("world"));
    }
}
