use std::path::Path;

use crate::error::AppError;
use crate::llm::types::FunctionCall;

use super::validate::{
    canonical_base, hidden_relative_path, relative_display, validate_path_containment,
    validate_removal_path, validate_target_path,
};

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

/// Like `truncate_for_display`, but says how much was cut instead of hiding it.
fn truncate_noted(s: &str, max_chars: usize) -> String {
    let total = s.chars().count();
    if total <= max_chars {
        return truncate_for_display(s, max_chars);
    }
    let head: String = s.chars().take(max_chars).collect();
    format!(
        "{}…({} more chars)",
        truncate_for_display(&head, max_chars),
        total - max_chars
    )
}

/// Multi-line preview of file content for the dialog: the first `max_lines` lines, at most
/// `max_chars` characters, control characters stripped, with a note on what was left out.
fn preview_text(s: &str, max_lines: usize, max_chars: usize) -> String {
    let clean: String = s
        .chars()
        .filter_map(|c| match c {
            '\n' => Some('\n'),
            '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    let total = clean.chars().count();
    let head = clean
        .split('\n')
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n");
    let head: String = head.chars().take(max_chars).collect();
    let shown = head.chars().count();
    if shown < total {
        format!("{head}\n…({} more chars)", total - shown)
    } else {
        head
    }
}

pub fn preview_paths(paths: &[String], max_items: usize) -> String {
    let mut lines: Vec<String> = paths
        .iter()
        .take(max_items)
        .map(|path| format!("- {}", truncate_for_display(path, 120)))
        .collect();
    if paths.len() > max_items {
        lines.push(format!(
            "- ... and {} more ({} total)",
            paths.len() - max_items,
            paths.len()
        ));
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
        lines.push(format!(
            "- ... and {} more ({} total)",
            operations.len() - max_items,
            operations.len()
        ));
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
                truncate_noted(search, 80),
                truncate_noted(replace, 80),
            )
        }
        _ => call.name.clone(),
    }
}

/// For `create_file` / `copy_file` (destination) / `create_directory`: the base-relative
/// path when the target sits under a hidden file or directory, which is what makes an
/// otherwise unconfirmed creation require confirmation. `None` for every other call.
pub fn hidden_write_target(
    call: &FunctionCall,
    base_path: &Path,
) -> Result<Option<String>, AppError> {
    let key = match call.name.as_str() {
        "create_file" | "create_directory" => "path",
        "copy_file" => "destination",
        _ => return Ok(None),
    };
    let Some(path) = call.arguments.get(key).and_then(|v| v.as_str()) else {
        // Missing argument: let execution report it.
        return Ok(None);
    };
    hidden_relative_path(path, base_path)
}

/// Base-relative display of a path argument that was already validated for this call.
fn display_target(path: &str, base_path: &Path) -> Result<String, AppError> {
    let resolved = validate_target_path(path, base_path)?;
    Ok(relative_display(&resolved, &canonical_base(base_path)?))
}

fn display_removal(path: &str, base_path: &Path) -> Result<String, AppError> {
    let resolved = validate_removal_path(path, base_path)?;
    Ok(relative_display(&resolved, &canonical_base(base_path)?))
}

fn str_arg<'a>(call: &'a FunctionCall, key: &str) -> Result<&'a str, AppError> {
    call.arguments
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::ToolValidation(format!("Missing required argument '{key}'")))
}

pub async fn build_confirmation_description(
    call: &FunctionCall,
    base_path: &Path,
) -> Result<String, AppError> {
    use super::dispatch::{matching_files_for_call, matching_rename_plan};
    use super::validate::{
        extract_bool, extract_rename_operations, extract_str_array, resolve_paths,
        resolve_rename_operations,
    };

    let args = &call.arguments;
    let base = canonical_base(base_path)?;
    let rel = |p: &String| relative_display(Path::new(p), &base);
    match call.name.as_str() {
        "delete_file" => {
            let path = str_arg(call, "path")?;
            let resolved = validate_removal_path(path, base_path)?;
            let shown = relative_display(&resolved, &base);
            let kind = match std::fs::symlink_metadata(&resolved) {
                Ok(m) if m.file_type().is_symlink() => " (symlink; only the link is removed)",
                Ok(m) if m.is_dir() => " (directory and everything inside it)",
                _ => "",
            };
            Ok(format!("Delete {shown}{kind}"))
        }
        "rename_file" => {
            let old = display_removal(str_arg(call, "old_path")?, base_path)?;
            let new = display_target(str_arg(call, "new_path")?, base_path)?;
            Ok(format!("Rename {old} → {new}"))
        }
        "edit_file" => {
            let path = str_arg(call, "path")?;
            let content = str_arg(call, "content")?;
            let shown = display_target(path, base_path)?;
            let resolved = validate_target_path(path, base_path)?;
            let current = std::fs::metadata(&resolved).map_or(0, |m| m.len());
            Ok(format!(
                "Overwrite file {shown}\n\nCurrent size: {current} bytes. New content: {} bytes, {} line(s).\n\nNew content preview:\n{}",
                content.len(),
                content.lines().count(),
                preview_text(content, 10, 600)
            ))
        }
        "patch_file" => {
            let path = str_arg(call, "path")?;
            let search = str_arg(call, "search")?;
            let replace = str_arg(call, "replace")?;
            let shown = display_target(path, base_path)?;
            Ok(format!(
                "Patch file {shown}\n\nFind:\n{}\n\nReplace with:\n{}",
                preview_text(search, 10, 600),
                preview_text(replace, 10, 600)
            ))
        }
        "create_file" | "create_directory" | "copy_file" => {
            let Some(hidden) = hidden_write_target(call, base_path)? else {
                return Ok(build_description(call));
            };
            match call.name.as_str() {
                "create_file" => {
                    let content = str_arg(call, "content")?;
                    Ok(format!(
                        "Create hidden file {hidden}\n\nContent ({} bytes):\n{}",
                        content.len(),
                        preview_text(content, 10, 600)
                    ))
                }
                "create_directory" => Ok(format!("Create hidden directory {hidden}")),
                _ => {
                    let source = validate_path_containment(str_arg(call, "source")?, base_path)?;
                    let source = relative_display(&source, &base);
                    Ok(format!("Copy {source} → hidden destination {hidden}"))
                }
            }
        }
        "delete_many" => {
            let paths = extract_str_array(args, "paths")?;
            let resolved = resolve_paths(paths, base_path)?;
            let shown: Vec<String> = resolved.iter().map(rel).collect();
            Ok(format!(
                "Delete {} item(s)?\n\n{}",
                shown.len(),
                preview_paths(&shown, 12)
            ))
        }
        "delete_matching" => {
            let path = str_arg(call, "path")?;
            let pattern = args
                .get("filename_regex")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let recursive = extract_bool(args, "recursive");
            let matches = matching_files_for_call(args, base_path).await?;
            let shown: Vec<String> = matches.iter().map(rel).collect();
            let dir = relative_display(&validate_path_containment(path, base_path)?, &base);
            Ok(format!(
                "Delete {} file(s) matching /{}/ under {} (recursive: {})?\n\n{}",
                shown.len(),
                truncate_for_display(pattern, 80),
                dir,
                recursive,
                preview_paths(&shown, 12)
            ))
        }
        "rename_many" => {
            let operations = extract_rename_operations(args)?;
            let resolved = resolve_rename_operations(operations, base_path)?;
            let shown: Vec<(String, String)> =
                resolved.iter().map(|(a, b)| (rel(a), rel(b))).collect();
            Ok(format!(
                "Rename {} item(s)?\n\n{}",
                shown.len(),
                preview_renames(&shown, 10)
            ))
        }
        "rename_matching" => {
            let path = str_arg(call, "path")?;
            let pattern = args
                .get("filename_regex")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let recursive = extract_bool(args, "recursive");
            let operations = matching_rename_plan(args, base_path).await?;
            let shown: Vec<(String, String)> =
                operations.iter().map(|(a, b)| (rel(a), rel(b))).collect();
            let dir = relative_display(&validate_path_containment(path, base_path)?, &base);
            Ok(format!(
                "Rename {} file(s) matching /{}/ under {} (recursive: {})?\n\n{}",
                shown.len(),
                truncate_for_display(pattern, 80),
                dir,
                recursive,
                preview_renames(&shown, 10)
            ))
        }
        _ => Ok(build_description(call)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_noted_reports_cut_length() {
        assert_eq!(truncate_noted("short", 80), "short");
        let out = truncate_noted(&"x".repeat(100), 80);
        assert!(out.ends_with("…(20 more chars)"), "{out}");
    }

    #[test]
    fn preview_text_limits_lines_and_chars_and_says_so() {
        let text = (1..=30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = preview_text(&text, 10, 600);
        assert!(out.starts_with("line 1\n"));
        assert!(out.contains("line 10"));
        assert!(!out.contains("line 11"));
        assert!(out.contains("more chars)"), "{out}");

        assert_eq!(preview_text("tiny", 10, 600), "tiny");
        let long = preview_text(&"é".repeat(1000), 10, 600);
        assert!(long.ends_with("…(400 more chars)"), "{long}");
    }

    #[test]
    fn preview_text_strips_control_characters() {
        assert_eq!(preview_text("a\x1b[31mb\tc", 10, 600), "a[31mb c");
    }

    #[test]
    fn bulk_previews_state_total_when_truncated() {
        let paths: Vec<String> = (0..20).map(|i| format!("f{i}")).collect();
        assert!(preview_paths(&paths, 12).ends_with("- ... and 8 more (20 total)"));
        let ops: Vec<(String, String)> = (0..15)
            .map(|i| (format!("a{i}"), format!("b{i}")))
            .collect();
        assert!(preview_renames(&ops, 10).ends_with("- ... and 5 more (15 total)"));
    }
}
