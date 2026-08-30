use anyhow::{anyhow, Result};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::process::Command;

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

pub async fn execute_tool(
    name: &str,
    arguments: &str,
) -> Result<serde_json::Value> {
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
            let mode = args
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("write");

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
            let dirpath = args
                .get("dirpath")
                .and_then(|v| v.as_str())
                .unwrap_or(".");

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
            let working_dir = args
                .get("working_dir")
                .and_then(|v| v.as_str());
            let timeout_secs = args
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(30);

            run_terminal_command(command, working_dir, timeout_secs)
        }
        _ => Err(anyhow!("Unknown tool: {}", name)),
    }
}

fn write_file(filepath: &str, content: &str, mode: &str) -> Result<serde_json::Value> {
    let path = std::path::Path::new(filepath).canonicalize()?;
    let cwd = std::env::current_dir()?;
    let home = home::home_dir().unwrap_or_default();

    // Security check
    if !path.starts_with(&cwd) && !path.starts_with(&home) {
        return Ok(json!({
            "success": false,
            "error": format!("Security: cannot write outside {:?}", cwd)
        }));
    }

    // Create directories if needed
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
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

fn run_terminal_command(command: &str, working_dir: Option<&str>, _timeout_secs: u64) -> Result<serde_json::Value> {
    use std::process::Stdio;

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

    let mut cmd = Command::new(shell);
    cmd.arg(shell_arg)
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

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

    let output = match cmd.output() {
        Ok(output) => output,
        Err(e) => {
            return Ok(json!({
                "success": false,
                "error": format!("Failed to execute command: {}", e)
            }));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    Ok(json!({
        "success": output.status.success(),
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr
    }))
}
