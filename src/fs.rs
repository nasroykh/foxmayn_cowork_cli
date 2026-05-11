use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use tokio::fs;

use crate::error::AppError;

const MAX_READ_BYTES: u64 = 5_242_880; // 5 MB

/// Directories skipped during recursive traversal (build artifacts, package managers,
/// bytecode caches). Hidden directories (starting with `.`) are skipped separately.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "__pycache__",
    "dist",
    "build",
    "vendor",
    ".git",
];

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
}

pub async fn list_files(path: String) -> Result<Vec<FileEntry>, AppError> {
    let mut entries = Vec::new();
    let mut dir = fs::read_dir(&path).await?;

    while let Some(entry) = dir.next_entry().await? {
        let metadata = entry.metadata().await?;
        entries.push(FileEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: entry.path().to_string_lossy().into_owned(),
            is_dir: metadata.is_dir(),
            size: metadata.len(),
        });
    }

    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    Ok(entries)
}

pub async fn read_file(path: String) -> Result<String, AppError> {
    let metadata = fs::metadata(&path).await?;
    if metadata.len() > MAX_READ_BYTES {
        return Err(AppError::ToolValidation(format!(
            "File is too large to read ({} bytes, max 5 MB)",
            metadata.len()
        )));
    }
    let content = fs::read_to_string(&path).await?;
    Ok(content)
}

pub async fn read_pdf(path: String) -> Result<String, AppError> {
    const MAX_PDF_BYTES: u64 = 52_428_800; // 50 MB
    let metadata = fs::metadata(&path).await?;
    if metadata.len() > MAX_PDF_BYTES {
        return Err(AppError::ToolValidation(format!(
            "PDF is too large ({} bytes, max 50 MB)",
            metadata.len()
        )));
    }
    let bytes = tokio::fs::read(&path).await?;
    tokio::task::spawn_blocking(move || {
        pdf_extract::extract_text_from_mem(&bytes)
            .map_err(|e| AppError::ToolValidation(format!("Failed to extract PDF text: {e}")))
    })
    .await
    .map_err(|e| AppError::ToolValidation(format!("PDF extraction task failed: {e}")))?
}

pub async fn create_file(path: String, content: String) -> Result<(), AppError> {
    if fs::try_exists(&path).await? {
        return Err(AppError::ToolValidation(format!(
            "File already exists: {path}. Use edit_file to overwrite."
        )));
    }
    if let Some(parent) = Path::new(&path).parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(&path, content).await?;
    Ok(())
}

pub async fn edit_file(path: String, content: String) -> Result<(), AppError> {
    if !fs::try_exists(&path).await? {
        return Err(AppError::ToolValidation(format!(
            "File does not exist: {path}. Use create_file to create a new file."
        )));
    }
    fs::write(&path, content).await?;
    Ok(())
}

pub async fn delete_file(path: String) -> Result<(), AppError> {
    let metadata = fs::metadata(&path).await?;
    if metadata.is_dir() {
        fs::remove_dir_all(&path).await?;
    } else {
        fs::remove_file(&path).await?;
    }
    Ok(())
}

pub async fn delete_many(paths: Vec<String>) -> Result<usize, AppError> {
    if paths.is_empty() {
        return Err(AppError::ToolValidation(
            "No paths were provided for bulk delete".into(),
        ));
    }

    // Validate all paths exist before touching any of them so a mid-batch
    // failure doesn't leave the filesystem in a partially-deleted state.
    for path in &paths {
        if !fs::try_exists(path).await? {
            return Err(AppError::ToolValidation(format!(
                "Path does not exist: {path}"
            )));
        }
    }

    let count = paths.len();
    for path in paths {
        delete_file(path).await?;
    }
    Ok(count)
}

pub async fn rename_file(old_path: String, new_path: String) -> Result<(), AppError> {
    fs::rename(&old_path, &new_path).await?;
    Ok(())
}

pub async fn rename_many(operations: Vec<(String, String)>) -> Result<usize, AppError> {
    if operations.is_empty() {
        return Err(AppError::ToolValidation(
            "No operations were provided for bulk rename".into(),
        ));
    }

    let mut destinations = HashSet::new();
    for (old_path, new_path) in &operations {
        if !fs::try_exists(old_path).await? {
            return Err(AppError::ToolValidation(format!(
                "Source does not exist: {old_path}"
            )));
        }
        if fs::try_exists(new_path).await? {
            return Err(AppError::ToolValidation(format!(
                "Destination already exists: {new_path}"
            )));
        }
        if !destinations.insert(new_path.clone()) {
            return Err(AppError::ToolValidation(format!(
                "Duplicate destination in rename plan: {new_path}"
            )));
        }
    }

    let count = operations.len();
    for (old_path, new_path) in operations {
        rename_file(old_path, new_path).await?;
    }
    Ok(count)
}

pub async fn create_directory(path: String) -> Result<(), AppError> {
    if fs::try_exists(&path).await? {
        return Err(AppError::ToolValidation(format!(
            "Path already exists: {path}"
        )));
    }
    fs::create_dir_all(&path).await?;
    Ok(())
}

pub async fn copy_file(source: String, destination: String) -> Result<(), AppError> {
    if fs::try_exists(&destination).await? {
        return Err(AppError::ToolValidation(format!(
            "Destination already exists: {destination}. Delete it first to overwrite."
        )));
    }
    if let Some(parent) = Path::new(&destination).parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::copy(&source, &destination).await?;
    Ok(())
}

pub async fn search_in_files(
    dir: String,
    pattern: String,
    max_results: usize,
) -> Result<String, AppError> {
    let re = regex::Regex::new(&pattern)
        .map_err(|e| AppError::ToolValidation(format!("Invalid regex pattern: {e}")))?;

    let mut results: Vec<String> = Vec::new();
    let mut total_matches: usize = 0;
    let mut dirs_to_visit = vec![dir];

    while let Some(current_dir) = dirs_to_visit.pop() {
        let Ok(mut entries) = fs::read_dir(&current_dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();

            if path.is_dir() {
                if !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_str()) {
                    dirs_to_visit.push(path.to_string_lossy().into_owned());
                }
            } else {
                // Skip files that exceed the read cap to avoid loading giant logs into RAM.
                let too_large = fs::metadata(&path)
                    .await
                    .map(|m| m.len() > MAX_READ_BYTES)
                    .unwrap_or(false);
                if too_large {
                    continue;
                }
                if let Ok(content) = fs::read_to_string(&path).await {
                    let file_str = path.to_string_lossy();
                    for (line_num, line) in content.lines().enumerate() {
                        if re.is_match(line) {
                            total_matches += 1;
                            if results.len() < max_results {
                                results.push(format!(
                                    "{}:{}: {}",
                                    file_str,
                                    line_num + 1,
                                    line.trim()
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    if results.is_empty() {
        return Ok("No matches found.".into());
    }

    let mut output = results.join("\n");
    if total_matches > max_results {
        output.push_str(&format!(
            "\n\n(showing first {max_results} of {total_matches} total matches)"
        ));
    }
    Ok(output)
}

pub async fn matching_files(
    dir: String,
    filename_regex: String,
    recursive: bool,
    max_matches: usize,
) -> Result<Vec<String>, AppError> {
    let re = regex::Regex::new(&filename_regex)
        .map_err(|e| AppError::ToolValidation(format!("Invalid filename regex: {e}")))?;

    let mut matches = Vec::new();
    let mut dirs_to_visit = vec![dir];

    while let Some(current_dir) = dirs_to_visit.pop() {
        let mut entries = fs::read_dir(&current_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = entry.file_type().await?;

            if file_type.is_dir() {
                if recursive && !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_str())
                {
                    dirs_to_visit.push(path.to_string_lossy().into_owned());
                }
            } else if file_type.is_file() && re.is_match(&name) {
                if matches.len() >= max_matches {
                    return Err(AppError::ToolValidation(format!(
                        "Matched more than {max_matches} files; narrow the pattern before running a bulk operation"
                    )));
                }
                matches.push(path.to_string_lossy().into_owned());
            }
        }
    }

    matches.sort();
    Ok(matches)
}

pub async fn patch_file(path: String, search: String, replace: String) -> Result<(), AppError> {
    let content = fs::read_to_string(&path).await?;
    let count = content.matches(&search).count();

    match count {
        0 => Err(AppError::ToolValidation(
            "Search text not found in file".into(),
        )),
        1 => {
            let new_content = content.replacen(&search, &replace, 1);
            fs::write(&path, new_content).await?;
            Ok(())
        }
        n => Err(AppError::ToolValidation(format!(
            "Search text is ambiguous: found {n} occurrences, expected exactly 1. \
             Use edit_file to overwrite the entire file."
        ))),
    }
}
