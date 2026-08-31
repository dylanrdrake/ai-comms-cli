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

pub async fn execute_tool(name: &str, arguments: &str) -> Result<serde_json::Value> {
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

            write_file(filepath, content, mode)
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

            replace_in_file(filepath, search, replace)
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

fn write_file(filepath: &str, content: &str, mode: &str) -> Result<serde_json::Value> {
    let cwd = std::env::current_dir()?;
    let home = home::home_dir().unwrap_or_default();

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

    // Create directories if needed, then canonicalize the (now-existing) parent
    // so the security check below still resolves symlinks/`..` even though the
    // target file itself may not exist yet.
    fs::create_dir_all(parent)?;
    let path = parent.canonicalize()?.join(file_name);

    // Security check
    if !path.starts_with(&cwd) && !path.starts_with(&home) {
        return Ok(json!({
            "success": false,
            "error": format!("Security: cannot write outside {:?}", cwd)
        }));
    }

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

fn replace_in_file(filepath: &str, search: &str, replace: &str) -> Result<serde_json::Value> {
    let path = Path::new(filepath);

    if !path.exists() {
        return Ok(json!({
            "success": false,
            "error": format!("File not found: {}", filepath)
        }));
    }

    let mut content = fs::read_to_string(path)?;

    if !content.contains(search) {
        return Ok(json!({
            "success": false,
            "error": "Search string not found in file"
        }));
    }

    content = content.replace(search, replace);
    fs::write(path, content)?;

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
