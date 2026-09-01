use anyhow::{anyhow, Result};
use serde_json::json;
use std::fs;
use std::path::Path;

pub fn get_tool_definitions() -> Vec<serde_json::Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Write or update a local file with code or text content",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "filepath": {
                            "type": "string",
                            "description": "Relative or absolute path to the file to write"
                        },
                        "content": {
                            "type": "string",
                            "description": "The file content to write"
                        },
                        "mode": {
                            "type": "string",
                            "enum": ["write", "append"],
                            "description": "write: overwrite the file, append: add to the end"
                        }
                    },
                    "required": ["filepath", "content"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read the contents of a local file",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "filepath": {
                            "type": "string",
                            "description": "Relative or absolute path to the file to read"
                        }
                    },
                    "required": ["filepath"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_files",
                "description": "List files in a directory",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "dirpath": {
                            "type": "string",
                            "description": "Directory path (default: current directory)"
                        }
                    }
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "replace_in_file",
                "description": "Replace text content in a file",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "filepath": {
                            "type": "string",
                            "description": "Path to the file to update"
                        },
                        "search": {
                            "type": "string",
                            "description": "Text to search for"
                        },
                        "replace": {
                            "type": "string",
                            "description": "Text to replace with"
                        }
                    },
                    "required": ["filepath", "search", "replace"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "run_terminal_command",
                "description": "Execute a shell command in the terminal and return the output",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute"
                        },
                        "working_dir": {
                            "type": "string",
                            "description": "Working directory for the command (default: current directory)"
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "description": "Timeout in seconds (default: 30)"
                        }
                    },
                    "required": ["command"]
                }
            }
        }),
    ]
}

/// Runs one tool call. `sandbox` is the session's current setting: with it
/// on, the tools that write are confined to the working directory and the
/// user's home. Reads are not bounded either way — they mutate nothing, and
/// confining them would break ordinary work like reading a file under
/// `/etc`.
pub async fn execute_tool(name: &str, arguments: &str, sandbox: bool) -> Result<serde_json::Value> {
    let args: serde_json::Value = serde_json::from_str(arguments)?;

    match name {
        "write_file" => {
            let filepath = args
                .get("filepath")
                .and_then(|v| v.as_str())
                .ok_or(anyhow!("Missing filepath"))?;
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or(anyhow!("Missing content"))?;
            let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("write");

            write_file(filepath, content, mode, sandbox)
        }
        "read_file" => {
            let filepath = args
                .get("filepath")
                .and_then(|v| v.as_str())
                .ok_or(anyhow!("Missing filepath"))?;

            read_file(filepath)
        }
        "list_files" => {
            let dirpath = args.get("dirpath").and_then(|v| v.as_str()).unwrap_or(".");

            list_files(dirpath)
        }
        "replace_in_file" => {
            let filepath = args
                .get("filepath")
                .and_then(|v| v.as_str())
                .ok_or(anyhow!("Missing filepath"))?;
            let search = args
                .get("search")
                .and_then(|v| v.as_str())
                .ok_or(anyhow!("Missing search"))?;
            let replace = args
                .get("replace")
                .and_then(|v| v.as_str())
                .ok_or(anyhow!("Missing replace"))?;

            replace_in_file(filepath, search, replace, sandbox)
        }
        "run_terminal_command" => {
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or(anyhow!("Missing command"))?;
            let working_dir = args.get("working_dir").and_then(|v| v.as_str());
            let timeout_secs = args
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(30);

            run_terminal_command(command, working_dir, timeout_secs).await
        }
        _ => Err(anyhow!("Unknown tool: {}", name)),
    }
}

/// The directories a write may land in: the working directory and the user's
/// home.
///
/// Both are canonicalized, because `path` is — and on Windows the two forms
/// don't compare. `canonicalize` there returns an extended-length path
/// (`\\?\D:\a\project`) while `current_dir` returns a plain one
/// (`D:\a\project`), so a prefix test between them never matches and the
/// sandbox refused *every* write, including the ones inside the working
/// directory it was meant to allow.
///
/// A bound that won't canonicalize falls back to its raw form rather than
/// being dropped: losing one would silently shrink what's allowed.
fn sandbox_bounds() -> Vec<std::path::PathBuf> {
    [std::env::current_dir().ok(), home::home_dir()]
        .into_iter()
        .flatten()
        .map(|dir| dir.canonicalize().unwrap_or(dir))
        .collect()
}

/// Resolves `filepath` to the absolute path a write would land on, without
/// requiring it to exist and without creating anything.
///
/// Canonicalizes the closest ancestor that *does* exist and re-joins the
/// rest, so `..` and symlinks are resolved as far as the filesystem can
/// resolve them — the bound is about where a write lands, not how it was
/// spelled. Creating nothing matters: `write_file` used to `create_dir_all`
/// before it checked, so a refused write still left directories behind
/// outside the sandbox.
fn resolve_for_sandbox(filepath: &str) -> Result<std::path::PathBuf> {
    let raw = Path::new(filepath);
    let absolute = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        std::env::current_dir()?.join(raw)
    };

    let mut existing = absolute.as_path();
    while !existing.exists() {
        match existing.parent() {
            Some(parent) => existing = parent,
            // Nothing on the path exists; judge it as spelled.
            None => return Ok(absolute.clone()),
        }
    }
    let canonical = existing
        .canonicalize()
        .unwrap_or_else(|_| existing.to_path_buf());
    Ok(match absolute.strip_prefix(existing) {
        // Nothing left to append: joining an empty component would add a
        // trailing separator, and a regular file path with one on the end
        // fails to `exists()` at all.
        Ok(rest) if rest.as_os_str().is_empty() => canonical,
        Ok(rest) => canonical.join(rest),
        Err(_) => canonical,
    })
}

/// The refusal to hand back when `path` is outside what the sandbox allows,
/// or `None` when the write may go ahead.
///
/// The bound is the working directory or the user's home. `path` must
/// already be canonicalized — resolving `..` and symlinks is what makes this
/// a check on where a write lands rather than on how it was spelled.
///
/// With `sandbox` off there is no bound at all; the refusal names the
/// setting so the way out of it is visible from the error itself.
fn sandbox_refusal(path: &Path, sandbox: bool) -> Option<serde_json::Value> {
    if !sandbox {
        return None;
    }
    if sandbox_bounds().iter().any(|bound| path.starts_with(bound)) {
        return None;
    }
    Some(json!({
        "success": false,
        "error": format!(
            "Sandbox: {} is outside the working directory and home directory. \
             Allow writes anywhere with /sandbox off (or comms sandbox off).",
            path.display()
        )
    }))
}

fn write_file(
    filepath: &str,
    content: &str,
    mode: &str,
    sandbox: bool,
) -> Result<serde_json::Value> {
    let cwd = std::env::current_dir()?;

    let raw_path = std::path::Path::new(filepath);
    let absolute = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        cwd.join(raw_path)
    };

    let parent = absolute
        .parent()
        .ok_or_else(|| anyhow!("Invalid file path: {}", filepath))?;
    let file_name = absolute
        .file_name()
        .ok_or_else(|| anyhow!("Invalid file path: {}", filepath))?;

    // Judged before anything is created, so a refused write leaves nothing
    // behind — not even the directories it would have needed.
    if let Some(refusal) = sandbox_refusal(&resolve_for_sandbox(filepath)?, sandbox) {
        return Ok(refusal);
    }

    fs::create_dir_all(parent)?;
    let path = parent.canonicalize()?.join(file_name);

    if mode == "append" {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        std::io::Write::write_all(&mut file, content.as_bytes())?;
    } else {
        fs::write(&path, content)?;
    }

    Ok(json!({
        "success": true,
        "message": format!("File written: {}", filepath),
        "filepath": path.to_string_lossy()
    }))
}

fn read_file(filepath: &str) -> Result<serde_json::Value> {
    let path = std::path::Path::new(filepath);

    if !path.exists() {
        return Ok(json!({
            "success": false,
            "error": format!("File not found: {}", filepath)
        }));
    }

    let content = fs::read_to_string(path)?;
    let lines = content.lines().count();

    Ok(json!({
        "success": true,
        "content": content,
        "lines": lines
    }))
}

fn list_files(dirpath: &str) -> Result<serde_json::Value> {
    let path = Path::new(dirpath);

    if !path.exists() {
        return Ok(json!({
            "success": false,
            "error": format!("Directory not found: {}", dirpath)
        }));
    }

    let mut files = vec![];
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        let is_dir = entry.path().is_dir();
        let display = if is_dir {
            format!("{}/", name.to_string_lossy())
        } else {
            name.to_string_lossy().to_string()
        };
        files.push(display);
    }

    files.sort();

    Ok(json!({
        "success": true,
        "files": files,
        "count": files.len()
    }))
}

fn replace_in_file(
    filepath: &str,
    search: &str,
    replace: &str,
    sandbox: bool,
) -> Result<serde_json::Value> {
    // The bound comes before the existence check, so a path outside the
    // sandbox is refused on its own terms rather than reporting whether a
    // file happens to be there.
    let path = resolve_for_sandbox(filepath)?;
    if let Some(refusal) = sandbox_refusal(&path, sandbox) {
        return Ok(refusal);
    }

    if !path.exists() {
        return Ok(json!({
            "success": false,
            "error": format!("File not found: {}", filepath)
        }));
    }

    let mut content = fs::read_to_string(&path)?;

    if !content.contains(search) {
        return Ok(json!({
            "success": false,
            "error": "Search string not found in file"
        }));
    }

    content = content.replace(search, replace);
    fs::write(&path, content)?;

    Ok(json!({
        "success": true,
        "message": "File updated"
    }))
}

async fn run_terminal_command(
    command: &str,
    working_dir: Option<&str>,
    timeout_secs: u64,
) -> Result<serde_json::Value> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;
    use tokio::process::Command as TokioCommand;
    use tokio::time::{timeout, Duration};

    let shell = if cfg!(target_os = "windows") {
        "cmd"
    } else {
        "sh"
    };

    let shell_arg = if cfg!(target_os = "windows") {
        "/C"
    } else {
        "-c"
    };

    let mut cmd = TokioCommand::new(shell);
    cmd.arg(shell_arg)
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Without this, cancelling a turn mid-tool-call drops the Child
        // without killing it, leaving an orphaned shell process running with
        // nothing watching it. The timeout path kills explicitly; this covers
        // the task simply being dropped.
        .kill_on_drop(true);

    if let Some(dir) = working_dir {
        let path = Path::new(dir);
        if !path.exists() {
            return Ok(json!({
                "success": false,
                "error": format!("Working directory not found: {}", dir)
            }));
        }
        cmd.current_dir(path);
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return Ok(json!({
                "success": false,
                "error": format!("Failed to execute command: {}", e)
            }));
        }
    };

    // Take the pipes and drain them concurrently with waiting on the child,
    // so a timeout can still `kill()` the child without losing ownership of
    // (and deadlocking on) its stdout/stderr.
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();

    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf).await;
        }
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf).await;
        }
        buf
    });

    let wait_result = timeout(Duration::from_secs(timeout_secs.max(1)), child.wait()).await;

    let status = match wait_result {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => {
            stdout_task.abort();
            stderr_task.abort();
            return Ok(json!({
                "success": false,
                "error": format!("Failed to execute command: {}", e)
            }));
        }
        Err(_) => {
            let _ = child.kill().await;
            stdout_task.abort();
            stderr_task.abort();
            return Ok(json!({
                "success": false,
                "error": format!(
                    "Command timed out after {} seconds and was killed",
                    timeout_secs
                ),
                "timed_out": true
            }));
        }
    };

    let stdout = stdout_task.await.unwrap_or_default();
    let stderr = stderr_task.await.unwrap_or_default();
    let exit_code = status.code().unwrap_or(-1);

    Ok(json!({
        "success": status.success(),
        "exit_code": exit_code,
        "stdout": String::from_utf8_lossy(&stdout).to_string(),
        "stderr": String::from_utf8_lossy(&stderr).to_string()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path that resolves outside both the working directory and home on
    /// every platform, and exists nowhere.
    ///
    /// Deliberately not `std::env::temp_dir()`: on Windows that sits under
    /// the user profile (`C:\\Users\\...\\AppData\\Local\\Temp`) — *inside*
    /// the sandbox — so a test built on it would assert a refusal that
    /// correctly never comes. A root-relative path lands on the current
    /// drive's root instead, outside both bounds everywhere.
    fn outside_the_sandbox() -> String {
        format!("/comms-sandbox-should-never-exist-{}/x", std::process::id())
    }

    #[test]
    fn replace_in_file_refuses_to_write_outside_the_sandbox() {
        // The gap this closes: `replace_in_file` had no bound at all, so it
        // could rewrite any existing file the process could open, while
        // `write_file` beside it was checked.
        let result = replace_in_file(&outside_the_sandbox(), "a", "b", true).unwrap();

        assert_eq!(result["success"], false);
        assert!(
            result["error"].as_str().unwrap().contains("Sandbox"),
            "{result}"
        );
    }

    #[test]
    fn the_bound_is_judged_before_whether_the_file_is_even_there() {
        // With the sandbox off the same path gets past the bound and fails
        // on its own terms, which is how this knows the refusal above came
        // from the bound rather than from the file simply being missing.
        let result = replace_in_file(&outside_the_sandbox(), "a", "b", false).unwrap();

        assert_eq!(result["success"], false);
        assert!(
            result["error"].as_str().unwrap().contains("File not found"),
            "{result}"
        );
    }

    #[test]
    fn replace_in_file_rewrites_a_file_inside_the_workspace() {
        let name = format!("comms-sandbox-test-{}-replace.txt", std::process::id());
        fs::write(&name, "before").unwrap();

        let result = replace_in_file(&name, "before", "after", true).unwrap();

        assert_eq!(result["success"], true, "{result}");
        assert_eq!(fs::read_to_string(&name).unwrap(), "after");
        fs::remove_file(&name).ok();
    }

    #[test]
    fn write_file_refuses_outside_the_sandbox_and_allows_inside_it() {
        let outside = outside_the_sandbox();
        let refused = write_file(&outside, "x", "write", true).unwrap();
        assert_eq!(refused["success"], false, "{refused}");
        // Refused before anything was created — not even the directory the
        // write would have needed.
        assert!(!Path::new(&outside).parent().unwrap().exists());

        // A relative path resolves against the working directory, which is
        // inside the bound.
        let inside = format!("comms-sandbox-test-{}-write.txt", std::process::id());
        let allowed = write_file(&inside, "x", "write", true).unwrap();
        assert_eq!(allowed["success"], true, "{allowed}");
        fs::remove_file(&inside).ok();
    }

    #[test]
    fn the_bound_is_where_a_path_lands_not_how_it_is_spelled() {
        // Canonicalization is what makes this true: a path that walks out of
        // the workspace with `..` is judged on where it ends up.
        let escape = format!(
            "{}/../../../../../../comms-sandbox-should-never-exist",
            std::env::current_dir().unwrap().display()
        );
        let result = write_file(&escape, "x", "write", true).unwrap();
        assert_eq!(result["success"], false, "{result}");
    }

    #[tokio::test]
    async fn run_terminal_command_returns_stdout_and_exit_code() {
        let result = run_terminal_command("echo hello", None, 5).await.unwrap();
        assert_eq!(result["success"], true);
        assert_eq!(result["exit_code"], 0);
        assert_eq!(result["stdout"].as_str().unwrap().trim(), "hello");
    }

    #[tokio::test]
    async fn run_terminal_command_reports_nonzero_exit() {
        let result = run_terminal_command("exit 3", None, 5).await.unwrap();
        assert_eq!(result["success"], false);
        assert_eq!(result["exit_code"], 3);
    }

    #[tokio::test]
    async fn run_terminal_command_enforces_timeout() {
        let result = run_terminal_command("sleep 5", None, 1).await.unwrap();
        assert_eq!(result["success"], false);
        assert_eq!(result["timed_out"], true);
    }

    #[tokio::test]
    async fn run_terminal_command_missing_working_dir_errors() {
        let result = run_terminal_command("echo hi", Some("/no/such/dir"), 5)
            .await
            .unwrap();
        assert_eq!(result["success"], false);
    }
}
