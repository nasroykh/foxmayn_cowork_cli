use std::path::{Path, PathBuf};

use serde::Serialize;

use super::types::{FunctionCall, Tool, ToolFunction};
use crate::error::AppError;

pub fn tool_definitions() -> Vec<Tool> {
    vec![
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "list_files".into(),
                description: "List files and directories at the given path".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to list. Use a relative path (e.g. 'src') or the exact working directory path. Relative paths are resolved against the working directory." }
                    },
                    "required": ["path"]
                }),
            },
        },
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "read_file".into(),
                description: "Read the contents of a file".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file. Prefer relative paths (e.g. 'src/main.rs'). Relative paths are resolved against the working directory." }
                    },
                    "required": ["path"]
                }),
            },
        },
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "create_file".into(),
                description: "Create a new file with content. Fails if the file already exists."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path for the new file. Prefer relative paths (e.g. 'notes.txt'). Relative paths are resolved against the working directory." },
                        "content": { "type": "string", "description": "Content to write into the file" }
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "edit_file".into(),
                description: "Overwrite an existing file with new content".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file to overwrite. Prefer relative paths (e.g. 'src/config.rs'). Relative paths are resolved against the working directory." },
                        "content": { "type": "string", "description": "New content for the file" }
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "delete_file".into(),
                description: "Delete a file or directory".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file or directory to delete. Prefer relative paths. Relative paths are resolved against the working directory." }
                    },
                    "required": ["path"]
                }),
            },
        },
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "rename_file".into(),
                description: "Rename or move a file or directory".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "old_path": { "type": "string", "description": "Current path (relative or absolute within working directory)" },
                        "new_path": { "type": "string", "description": "New path (relative or absolute within working directory)" }
                    },
                    "required": ["old_path", "new_path"]
                }),
            },
        },
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "create_directory".into(),
                description: "Create a new directory. Parent directories are created as needed. Fails if the path already exists.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path for the new directory. Prefer relative paths (e.g. 'src/utils'). Relative paths are resolved against the working directory." }
                    },
                    "required": ["path"]
                }),
            },
        },
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "copy_file".into(),
                description: "Copy a file to a new location. Fails if the destination already exists.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "source": { "type": "string", "description": "Source file path (relative or absolute within working directory)" },
                        "destination": { "type": "string", "description": "Destination file path (relative or absolute within working directory)" }
                    },
                    "required": ["source", "destination"]
                }),
            },
        },
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "search_in_files".into(),
                description: "Search for a regex pattern in all files under a directory. Returns matching lines with file paths and line numbers. Skips hidden directories and build artifacts.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory to search in. Prefer relative paths. Relative paths are resolved against the working directory." },
                        "pattern": { "type": "string", "description": "Regex pattern to search for (e.g. 'fn main', 'TODO', 'error\\(')" }
                    },
                    "required": ["path", "pattern"]
                }),
            },
        },
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "patch_file".into(),
                description: "Replace a specific text occurrence in a file. The search text must match exactly once — fails on 0 or multiple matches. Prefer this over edit_file for small targeted changes.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file to patch. Prefer relative paths." },
                        "search": { "type": "string", "description": "Exact text to find. Must appear exactly once in the file." },
                        "replace": { "type": "string", "description": "Replacement text." }
                    },
                    "required": ["path", "search", "replace"]
                }),
            },
        },
    ]
}

const DESTRUCTIVE_OPS: &[&str] = &["delete_file", "edit_file", "patch_file"];

#[derive(Debug, Serialize, Clone)]
pub struct ToolCallResult {
    pub result: Option<String>,
    pub requires_confirmation: bool,
    pub description: String,
    pub tool_name: String,
    pub args: serde_json::Value,
}

fn extract_str(args: &serde_json::Value, key: &str) -> Result<String, AppError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| AppError::ToolValidation(format!("Missing required argument '{key}'")))
}

fn validate_path_containment(path: &str, base_path: &Path) -> Result<PathBuf, AppError> {
    let target = PathBuf::from(path);

    if target
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return Err(AppError::ToolValidation(format!(
            "Path '{path}' contains '..' components which are not allowed"
        )));
    }

    let canonical_base = base_path
        .canonicalize()
        .map_err(|_| AppError::ToolValidation("Working directory not accessible".into()))?;

    let resolved = if target.is_absolute() {
        target
    } else {
        canonical_base.join(&target)
    };

    let check_path = if resolved.exists() {
        resolved.canonicalize()?
    } else {
        resolved
    };

    if !check_path.starts_with(&canonical_base) {
        return Err(AppError::ToolValidation(format!(
            "Path '{path}' is outside the working directory"
        )));
    }

    Ok(check_path)
}

pub async fn dispatch_tool_call(
    call: &FunctionCall,
    base_path: &Path,
) -> Result<ToolCallResult, AppError> {
    let known_tools = [
        "list_files",
        "read_file",
        "create_file",
        "edit_file",
        "delete_file",
        "rename_file",
        "create_directory",
        "copy_file",
        "search_in_files",
        "patch_file",
    ];
    if !known_tools.contains(&call.name.as_str()) {
        return Err(AppError::ToolValidation(format!(
            "Unknown tool '{}'. Available: {}",
            call.name,
            known_tools.join(", ")
        )));
    }

    let is_destructive = DESTRUCTIVE_OPS.contains(&call.name.as_str());

    if is_destructive {
        let description = build_description(call);
        return Ok(ToolCallResult {
            result: None,
            requires_confirmation: true,
            description,
            tool_name: call.name.clone(),
            args: call.arguments.clone(),
        });
    }

    let result = execute_tool(call, base_path).await?;

    Ok(ToolCallResult {
        description: build_description(call),
        result: Some(result),
        requires_confirmation: false,
        tool_name: call.name.clone(),
        args: call.arguments.clone(),
    })
}

pub async fn execute_tool(call: &FunctionCall, base_path: &Path) -> Result<String, AppError> {
    let args = &call.arguments;

    match call.name.as_str() {
        "list_files" => {
            let path = extract_str(args, "path")?;
            let resolved = validate_path_containment(&path, base_path)?
                .to_string_lossy()
                .into_owned();
            let entries = crate::fs::list_files(resolved).await?;
            Ok(serde_json::to_string_pretty(&entries)?)
        }
        "read_file" => {
            let path = extract_str(args, "path")?;
            let resolved = validate_path_containment(&path, base_path)?
                .to_string_lossy()
                .into_owned();
            crate::fs::read_file(resolved).await
        }
        "create_file" => {
            let path = extract_str(args, "path")?;
            let content = extract_str(args, "content")?;
            let resolved = validate_path_containment(&path, base_path)?
                .to_string_lossy()
                .into_owned();
            crate::fs::create_file(resolved, content).await?;
            Ok("File created successfully".into())
        }
        "edit_file" => {
            let path = extract_str(args, "path")?;
            let content = extract_str(args, "content")?;
            let resolved = validate_path_containment(&path, base_path)?
                .to_string_lossy()
                .into_owned();
            crate::fs::edit_file(resolved, content).await?;
            Ok("File updated successfully".into())
        }
        "delete_file" => {
            let path = extract_str(args, "path")?;
            let resolved = validate_path_containment(&path, base_path)?
                .to_string_lossy()
                .into_owned();
            crate::fs::delete_file(resolved).await?;
            Ok("Deleted successfully".into())
        }
        "rename_file" => {
            let old_path = extract_str(args, "old_path")?;
            let new_path = extract_str(args, "new_path")?;
            let resolved_old = validate_path_containment(&old_path, base_path)?
                .to_string_lossy()
                .into_owned();
            let resolved_new = validate_path_containment(&new_path, base_path)?
                .to_string_lossy()
                .into_owned();
            crate::fs::rename_file(resolved_old, resolved_new).await?;
            Ok("Renamed successfully".into())
        }
        "create_directory" => {
            let path = extract_str(args, "path")?;
            let resolved = validate_path_containment(&path, base_path)?
                .to_string_lossy()
                .into_owned();
            crate::fs::create_directory(resolved).await?;
            Ok("Directory created successfully".into())
        }
        "copy_file" => {
            let source = extract_str(args, "source")?;
            let destination = extract_str(args, "destination")?;
            let resolved_src = validate_path_containment(&source, base_path)?
                .to_string_lossy()
                .into_owned();
            let resolved_dst = validate_path_containment(&destination, base_path)?
                .to_string_lossy()
                .into_owned();
            crate::fs::copy_file(resolved_src, resolved_dst).await?;
            Ok("File copied successfully".into())
        }
        "search_in_files" => {
            let path = extract_str(args, "path")?;
            let pattern = extract_str(args, "pattern")?;
            let resolved = validate_path_containment(&path, base_path)?
                .to_string_lossy()
                .into_owned();
            crate::fs::search_in_files(resolved, pattern, 50).await
        }
        "patch_file" => {
            let path = extract_str(args, "path")?;
            let search = extract_str(args, "search")?;
            let replace = extract_str(args, "replace")?;
            let resolved = validate_path_containment(&path, base_path)?
                .to_string_lossy()
                .into_owned();
            crate::fs::patch_file(resolved, search, replace).await?;
            Ok("File patched successfully".into())
        }
        _ => Err(AppError::ToolValidation(format!(
            "Unknown tool '{}'",
            call.name
        ))),
    }
}

fn build_description(call: &FunctionCall) -> String {
    let args = &call.arguments;
    match call.name.as_str() {
        "list_files" => format!(
            "List files in {}",
            args.get("path").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        "read_file" => format!(
            "Read file {}",
            args.get("path").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        "create_file" => format!(
            "Create file {}",
            args.get("path").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        "edit_file" => format!(
            "Overwrite file {}",
            args.get("path").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        "delete_file" => format!(
            "Delete {}",
            args.get("path").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        "rename_file" => format!(
            "Rename {} → {}",
            args.get("old_path").and_then(|v| v.as_str()).unwrap_or("?"),
            args.get("new_path").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        "create_directory" => format!(
            "Create directory {}",
            args.get("path").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        "copy_file" => format!(
            "Copy {} → {}",
            args.get("source").and_then(|v| v.as_str()).unwrap_or("?"),
            args.get("destination").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        "search_in_files" => format!(
            "Search for '{}' in {}",
            args.get("pattern").and_then(|v| v.as_str()).unwrap_or("?"),
            args.get("path").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        "patch_file" => format!(
            "Patch file {} (search & replace)",
            args.get("path").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        _ => call.name.clone(),
    }
}
