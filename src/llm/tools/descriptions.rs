use crate::llm::types::FunctionCall;

/// Render a string for the confirmation popup: collapse newlines into a glyph
/// and cap the visible length so the dialog stays readable.
pub fn truncate_for_display(s: &str, max_chars: usize) -> String {
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

pub fn preview_paths(paths: &[String], max_items: usize) -> String {
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

pub fn preview_renames(operations: &[(String, String)], max_items: usize) -> String {
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

/// Short, user-friendly action for a tool name. Used by the `Default` display verbosity.
/// No arguments, no result — just what the tool conceptually did.
pub(crate) fn brief_action(tool_name: &str) -> &'static str {
    match tool_name {
        "list_files" => "Listed directory",
        "read_file" => "Read file",
        "read_pdf" => "Read PDF",
        "create_file" => "Created file",
        "edit_file" => "Overwrote file",
        "delete_file" => "Deleted item",
        "delete_many" => "Deleted items",
        "rename_file" => "Renamed item",
        "rename_many" => "Renamed items",
        "delete_matching" => "Deleted matching files",
        "rename_matching" => "Renamed matching files",
        "create_directory" => "Created directory",
        "copy_file" => "Copied file",
        "find_files" => "Searched for files",
        "search_in_files" => "Searched file contents",
        "patch_file" => "Patched file",
        _ => "Ran tool",
    }
}

pub fn build_description(call: &FunctionCall) -> String {
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
        "read_pdf" => format!(
            "Read PDF {}",
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
        "find_files" => format!(
            "Find files matching /{}/ in {}",
            args.get("filename_regex")
                .and_then(|v| v.as_str())
                .unwrap_or("?"),
            args.get("path").and_then(|v| v.as_str()).unwrap_or("?")
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

pub async fn build_confirmation_description(
    call: &FunctionCall,
    base_path: &std::path::Path,
) -> Result<String, crate::error::AppError> {
    use super::dispatch::{matching_files_for_call, matching_rename_plan};
    use super::validate::{
        extract_bool, extract_rename_operations, extract_str_array, resolve_paths,
        resolve_rename_operations,
    };

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
