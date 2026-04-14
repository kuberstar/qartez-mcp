use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct DetectedToolchain {
    pub name: String,
    pub build_tool: String,
    pub test_cmd: Vec<String>,
    pub build_cmd: Vec<String>,
    pub lint_cmd: Option<Vec<String>>,
    pub typecheck_cmd: Option<Vec<String>>,
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
        });
    }

    toolchains
}

/// Checks whether a binary is available on PATH.
pub fn binary_available(name: &str) -> bool {
    // Skip path-relative binaries like ./gradlew — they are project-local.
    if name.starts_with('.') || name.contains('/') {
        return true;
    }
    Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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

    let exit_code = child
        .wait()
        .ok()
        .and_then(|s| s.code())
        .unwrap_or(-1);
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
        fs::write(
            dir.path().join("pom.xml"),
            "<project></project>",
        )
        .unwrap();

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
