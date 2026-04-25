use std::path::{Path, PathBuf};

use serde::Serialize;

use super::types::{FunctionCall, Tool, ToolFunction};
use crate::error::AppError;

const MAX_BULK_MATCHES: usize = 200;

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
                name: "delete_many".into(),
                description: "Delete multiple explicit files or directories in one confirmed operation. Prefer this over repeated delete_file calls when the user asks to delete several known paths.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "paths": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Paths to delete. Prefer relative paths. Every path is resolved against the working directory and validated before execution."
                        }
                    },
                    "required": ["paths"]
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
                name: "rename_many".into(),
                description: "Rename or move multiple explicit files/directories in one confirmed operation. Fails before changing anything if a destination already exists or is duplicated.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "operations": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "old_path": { "type": "string", "description": "Current path within the working directory" },
                                    "new_path": { "type": "string", "description": "Destination path within the working directory" }
                                },
                                "required": ["old_path", "new_path"]
                            },
                            "description": "Rename/move operations to perform."
                        }
                    },
                    "required": ["operations"]
                }),
            },
        },
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "delete_matching".into(),
                description: "Delete files whose filename matches a regex under a directory in one confirmed operation. Use this for requests like deleting all .md files. Files only; directories are not deleted by this tool.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory to scan. Prefer a relative path." },
                        "filename_regex": { "type": "string", "description": "Regex applied to each filename, e.g. '\\\\.md$'." },
                        "recursive": { "type": "boolean", "description": "Whether to scan subdirectories. Defaults to false." }
                    },
                    "required": ["path", "filename_regex"]
                }),
            },
        },
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "rename_matching".into(),
                description: "Bulk rename files whose filename matches a regex under a directory. The replacement is applied to filenames only, not full paths. Fails before changing anything if a destination exists or is duplicated.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory to scan. Prefer a relative path." },
                        "filename_regex": { "type": "string", "description": "Regex applied to each filename, e.g. '\\\\s+' or '\\\\.jpeg$'." },
                        "replacement": { "type": "string", "description": "Regex replacement for the filename, e.g. '_' or '.jpg'." },
                        "recursive": { "type": "boolean", "description": "Whether to scan subdirectories. Defaults to false." }
                    },
                    "required": ["path", "filename_regex", "replacement"]
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

const DESTRUCTIVE_OPS: &[&str] = &[
    "delete_file",
    "delete_many",
    "delete_matching",
    "edit_file",
    "patch_file",
    "rename_file",
    "rename_many",
    "rename_matching",
];

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

fn extract_bool(args: &serde_json::Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn extract_str_array(args: &serde_json::Value, key: &str) -> Result<Vec<String>, AppError> {
    let values = args
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| AppError::ToolValidation(format!("Missing required array '{key}'")))?;

    values
        .iter()
        .map(|v| {
            v.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                AppError::ToolValidation(format!("Every item in '{key}' must be a string"))
            })
        })
        .collect()
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

fn resolve_paths(paths: Vec<String>, base_path: &Path) -> Result<Vec<String>, AppError> {
    if paths.is_empty() {
        return Err(AppError::ToolValidation("No paths were provided".into()));
    }

    paths
        .iter()
        .map(|path| {
            validate_path_containment(path, base_path).map(|p| p.to_string_lossy().into_owned())
        })
        .collect()
}

fn extract_rename_operations(args: &serde_json::Value) -> Result<Vec<(String, String)>, AppError> {
    let operations = args
        .get("operations")
        .and_then(|v| v.as_array())
        .ok_or_else(|| AppError::ToolValidation("Missing required array 'operations'".into()))?;

    if operations.is_empty() {
        return Err(AppError::ToolValidation(
            "No rename operations were provided".into(),
        ));
    }

    operations
        .iter()
        .map(|op| {
            let old_path = op.get("old_path").and_then(|v| v.as_str()).ok_or_else(|| {
                AppError::ToolValidation("Each rename operation needs 'old_path'".into())
            })?;
            let new_path = op.get("new_path").and_then(|v| v.as_str()).ok_or_else(|| {
                AppError::ToolValidation("Each rename operation needs 'new_path'".into())
            })?;
            Ok((old_path.to_owned(), new_path.to_owned()))
        })
        .collect()
}

fn resolve_rename_operations(
    operations: Vec<(String, String)>,
    base_path: &Path,
) -> Result<Vec<(String, String)>, AppError> {
    operations
        .iter()
        .map(|(old_path, new_path)| {
            let old_resolved = validate_path_containment(old_path, base_path)?
                .to_string_lossy()
                .into_owned();
            let new_resolved = validate_path_containment(new_path, base_path)?
                .to_string_lossy()
                .into_owned();
            Ok((old_resolved, new_resolved))
        })
        .collect()
}

async fn matching_files_for_call(
    args: &serde_json::Value,
    base_path: &Path,
) -> Result<Vec<String>, AppError> {
    let path = extract_str(args, "path")?;
    let filename_regex = extract_str(args, "filename_regex")?;
    let recursive = extract_bool(args, "recursive");
    let resolved = validate_path_containment(&path, base_path)?
        .to_string_lossy()
        .into_owned();
    let matches =
        crate::fs::matching_files(resolved, filename_regex, recursive, MAX_BULK_MATCHES).await?;
    if matches.is_empty() {
        return Err(AppError::ToolValidation(
            "No files matched the requested bulk operation".into(),
        ));
    }
    Ok(matches)
}

fn validate_generated_filename(name: &str) -> Result<(), AppError> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(AppError::ToolValidation(format!(
            "Bulk rename generated an invalid filename: {name:?}"
        )));
    }
    Ok(())
}

async fn matching_rename_plan(
    args: &serde_json::Value,
    base_path: &Path,
) -> Result<Vec<(String, String)>, AppError> {
    let filename_regex = extract_str(args, "filename_regex")?;
    let replacement = extract_str(args, "replacement")?;
    let re = regex::Regex::new(&filename_regex)
        .map_err(|e| AppError::ToolValidation(format!("Invalid filename regex: {e}")))?;
    let files = matching_files_for_call(args, base_path).await?;

    let mut operations = Vec::new();
    for old_path in files {
        let path = PathBuf::from(&old_path);
        let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let new_name = re.replace_all(file_name, replacement.as_str()).to_string();
        validate_generated_filename(&new_name)?;
        if new_name == file_name {
            continue;
        }
        let Some(parent) = path.parent() else {
            continue;
        };
        let new_path = parent.join(new_name).to_string_lossy().into_owned();
        validate_path_containment(&new_path, base_path)?;
        operations.push((old_path, new_path));
    }

    if operations.is_empty() {
        return Err(AppError::ToolValidation(
            "Bulk rename matched files but produced no filename changes".into(),
        ));
    }

    Ok(operations)
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
        "delete_many",
        "rename_file",
        "rename_many",
        "delete_matching",
        "rename_matching",
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
        let description = build_confirmation_description(call, base_path).await?;
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
        "delete_many" => {
            let paths = extract_str_array(args, "paths")?;
            let resolved = resolve_paths(paths, base_path)?;
            let count = crate::fs::delete_many(resolved).await?;
            Ok(format!("Deleted {count} item(s) successfully"))
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
        "rename_many" => {
            let operations = extract_rename_operations(args)?;
            let resolved = resolve_rename_operations(operations, base_path)?;
            let count = crate::fs::rename_many(resolved).await?;
            Ok(format!("Renamed {count} item(s) successfully"))
        }
        "delete_matching" => {
            let paths = matching_files_for_call(args, base_path).await?;
            let count = crate::fs::delete_many(paths).await?;
            Ok(format!("Deleted {count} matching file(s) successfully"))
        }
        "rename_matching" => {
            let operations = matching_rename_plan(args, base_path).await?;
            let count = crate::fs::rename_many(operations).await?;
            Ok(format!("Renamed {count} matching file(s) successfully"))
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

/// Render a string for the confirmation popup: collapse newlines into a glyph
/// and cap the visible length so the dialog stays readable.
fn truncate_for_display(s: &str, max_chars: usize) -> String {
    let flattened: String = s
        .chars()
        .map(|c| match c {
            '\n' => '↵',
            '\t' => ' ',
            _ => c,
        })
        .collect();
    if flattened.chars().count() <= max_chars {
        flattened
    } else {
        let mut out: String = flattened
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect();
        out.push('…');
        out
    }
}

fn preview_paths(paths: &[String], max_items: usize) -> String {
    let mut lines: Vec<String> = paths
        .iter()
        .take(max_items)
        .map(|path| format!("- {}", truncate_for_display(path, 120)))
        .collect();
    if paths.len() > max_items {
        lines.push(format!("- ... and {} more", paths.len() - max_items));
    }
    lines.join("\n")
}

fn preview_renames(operations: &[(String, String)], max_items: usize) -> String {
    let mut lines: Vec<String> = operations
        .iter()
        .take(max_items)
        .map(|(old_path, new_path)| {
            format!(
                "- {} → {}",
                truncate_for_display(old_path, 80),
                truncate_for_display(new_path, 80)
            )
        })
        .collect();
    if operations.len() > max_items {
        lines.push(format!("- ... and {} more", operations.len() - max_items));
    }
    lines.join("\n")
}

async fn build_confirmation_description(
    call: &FunctionCall,
    base_path: &Path,
) -> Result<String, AppError> {
    let args = &call.arguments;
    match call.name.as_str() {
        "delete_many" => {
            let paths = extract_str_array(args, "paths")?;
            let resolved = resolve_paths(paths, base_path)?;
            Ok(format!(
                "Delete {} item(s)?\n\n{}",
                resolved.len(),
                preview_paths(&resolved, 12)
            ))
        }
        "delete_matching" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let pattern = args
                .get("filename_regex")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let recursive = extract_bool(args, "recursive");
            let matches = matching_files_for_call(args, base_path).await?;
            Ok(format!(
                "Delete {} file(s) matching /{}/ under {} (recursive: {})?\n\n{}",
                matches.len(),
                truncate_for_display(pattern, 80),
                path,
                recursive,
                preview_paths(&matches, 12)
            ))
        }
        "rename_many" => {
            let operations = extract_rename_operations(args)?;
            let resolved = resolve_rename_operations(operations, base_path)?;
            Ok(format!(
                "Rename {} item(s)?\n\n{}",
                resolved.len(),
                preview_renames(&resolved, 10)
            ))
        }
        "rename_matching" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let pattern = args
                .get("filename_regex")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let recursive = extract_bool(args, "recursive");
            let operations = matching_rename_plan(args, base_path).await?;
            Ok(format!(
                "Rename {} file(s) matching /{}/ under {} (recursive: {})?\n\n{}",
                operations.len(),
                truncate_for_display(pattern, 80),
                path,
                recursive,
                preview_renames(&operations, 10)
            ))
        }
        _ => Ok(build_description(call)),
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
        "delete_many" => {
            let count = args
                .get("paths")
                .and_then(|v| v.as_array())
                .map_or(0, Vec::len);
            format!("Delete {count} item(s)")
        }
        "rename_file" => format!(
            "Rename {} → {}",
            args.get("old_path").and_then(|v| v.as_str()).unwrap_or("?"),
            args.get("new_path").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        "rename_many" => {
            let count = args
                .get("operations")
                .and_then(|v| v.as_array())
                .map_or(0, Vec::len);
            format!("Rename {count} item(s)")
        }
        "delete_matching" => format!(
            "Delete files matching /{}/ in {}",
            args.get("filename_regex")
                .and_then(|v| v.as_str())
                .unwrap_or("?"),
            args.get("path").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        "rename_matching" => format!(
            "Rename files matching /{}/ in {}",
            args.get("filename_regex")
                .and_then(|v| v.as_str())
                .unwrap_or("?"),
            args.get("path").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        "create_directory" => format!(
            "Create directory {}",
            args.get("path").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        "copy_file" => format!(
            "Copy {} → {}",
            args.get("source").and_then(|v| v.as_str()).unwrap_or("?"),
            args.get("destination")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
        ),
        "search_in_files" => format!(
            "Search for '{}' in {}",
            args.get("pattern").and_then(|v| v.as_str()).unwrap_or("?"),
            args.get("path").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        "patch_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let search = args.get("search").and_then(|v| v.as_str()).unwrap_or("?");
            let replace = args.get("replace").and_then(|v| v.as_str()).unwrap_or("?");
            format!(
                "Patch file {path}\n\nFind:    {}\nReplace: {}",
                truncate_for_display(search, 80),
                truncate_for_display(replace, 80),
            )
        }
        _ => call.name.clone(),
    }
}

#[cfg(test)]
mod path_containment_tests {
    use super::*;
    use std::path::PathBuf;

    fn project_base() -> PathBuf {
        // Cargo runs tests from the package root; this is the project we're testing.
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn rejects_explicit_parent_dir_components() {
        let base = project_base();
        assert!(validate_path_containment("../escape", &base).is_err());
        assert!(validate_path_containment("../../escape", &base).is_err());
        assert!(validate_path_containment("foo/../bar", &base).is_err());
        assert!(validate_path_containment("./../escape", &base).is_err());
    }

    #[test]
    fn allows_relative_paths_within_base() {
        let base = project_base();
        // existing files
        assert!(validate_path_containment("src/main.rs", &base).is_ok());
        assert!(validate_path_containment("Cargo.toml", &base).is_ok());
        // non-existent but inside the base
        assert!(validate_path_containment("nonexistent_file_xyz.txt", &base).is_ok());
        assert!(validate_path_containment("nested/dir/that/does/not/exist.txt", &base).is_ok());
    }

    #[test]
    fn rejects_absolute_paths_outside_base() {
        let base = project_base();
        // /etc/passwd is unambiguously outside any user project directory
        assert!(validate_path_containment("/etc/passwd", &base).is_err());
        assert!(validate_path_containment("/tmp", &base).is_err());
        // even non-existent absolute paths outside should fail
        assert!(validate_path_containment("/this/path/does/not/exist/anywhere", &base).is_err());
    }

    #[test]
    fn allows_canonicalized_absolute_paths_inside_base() {
        let base = project_base();
        let inside = base.join("src/main.rs");
        let result = validate_path_containment(inside.to_string_lossy().as_ref(), &base);
        assert!(result.is_ok());
    }
}
