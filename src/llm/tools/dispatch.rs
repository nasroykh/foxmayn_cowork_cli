use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::AppError;
use crate::llm::types::FunctionCall;

use super::descriptions::{build_confirmation_description, build_description};
use super::validate::{
    extract_bool, extract_rename_operations, extract_str, extract_str_array,
    resolve_paths, resolve_rename_operations, validate_generated_filename,
    validate_path_containment,
};

pub const MAX_BULK_MATCHES: usize = 200;

pub const DESTRUCTIVE_OPS: &[&str] = &[
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

pub async fn matching_files_for_call(
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

pub async fn matching_rename_plan(
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
    // execute_tool's match is the single source of truth for known tools;
    // unknown names surface as an error there via the catch-all arm.
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
        "read_pdf" => {
            let path = extract_str(args, "path")?;
            let resolved = validate_path_containment(&path, base_path)?
                .to_string_lossy()
                .into_owned();
            crate::fs::read_pdf(resolved).await
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
        "find_files" => {
            let path = extract_str(args, "path")?;
            // filename_regex is optional — default to ".*" (match every filename) so
            // the model can enumerate every file without inventing a "match-all" regex.
            let filename_regex = args
                .get("filename_regex")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(".*")
                .to_owned();
            let recursive = extract_bool(args, "recursive");
            let resolved = validate_path_containment(&path, base_path)?
                .to_string_lossy()
                .into_owned();
            let matches =
                crate::fs::matching_files(resolved, filename_regex, recursive, MAX_BULK_MATCHES)
                    .await?;
            if matches.is_empty() {
                Ok("No matching files found.".into())
            } else {
                Ok(matches.join("\n"))
            }
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
