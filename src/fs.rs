use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::error::AppError;

const MAX_READ_BYTES: u64 = 5_242_880; // 5 MB
const MAX_PDF_TEXT_CHARS: usize = 1_000_000;
const MAX_SEARCH_LINE_CHARS: usize = 300;
/// Bytes inspected for a NUL to classify a file as binary.
const BINARY_SNIFF_BYTES: usize = 8192;

/// Directories skipped during recursive traversal (build artifacts, package managers,
/// bytecode caches, virtualenvs). Hidden directories (starting with `.`) are skipped
/// separately; the dot-names are listed too so the intent is explicit.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "__pycache__",
    "dist",
    "build",
    "vendor",
    "venv",
    ".git",
    ".venv",
    ".next",
    ".cache",
];

fn is_skipped_dir(name: &str) -> bool {
    name.starts_with('.') || SKIP_DIRS.contains(&name)
}

/// True when something exists at `path`, without following a final symlink (so a
/// dangling symlink counts as existing).
async fn exists_nofollow(path: &str) -> bool {
    fs::symlink_metadata(path).await.is_ok()
}

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
        // An entry can vanish between readdir and stat; skip it instead of failing the listing.
        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
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

/// Metadata of `path`, requiring a regular file. FIFOs, devices and sockets can block a
/// read forever, so they are refused.
async fn regular_file_metadata(path: &str) -> Result<std::fs::Metadata, AppError> {
    let metadata = fs::metadata(path).await?;
    if !metadata.is_file() {
        return Err(AppError::ToolValidation(format!(
            "Not a regular file: {path}"
        )));
    }
    Ok(metadata)
}

pub async fn read_file(path: String) -> Result<String, AppError> {
    let metadata = regular_file_metadata(&path).await?;
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
    let metadata = regular_file_metadata(&path).await?;
    if metadata.len() > MAX_PDF_BYTES {
        return Err(AppError::ToolValidation(format!(
            "PDF is too large ({} bytes, max 50 MB)",
            metadata.len()
        )));
    }
    let bytes = tokio::fs::read(&path).await?;
    let text = tokio::task::spawn_blocking(move || {
        pdf_extract::extract_text_from_mem(&bytes)
            .map_err(|e| AppError::ToolValidation(format!("Failed to extract PDF text: {e}")))
    })
    .await
    .map_err(|e| AppError::ToolValidation(format!("PDF extraction task failed: {e}")))??;
    Ok(cap_chars(
        text,
        MAX_PDF_TEXT_CHARS,
        "PDF text truncated at 1,000,000 characters",
    ))
}

/// Keep at most `max_chars` characters (on a char boundary), appending `note` when cut.
fn cap_chars(text: String, max_chars: usize, note: &str) -> String {
    match text.char_indices().nth(max_chars) {
        None => text,
        Some((cut, _)) => format!("{}\n\n[{note}]", &text[..cut]),
    }
}

pub async fn create_file(path: String, content: String) -> Result<(), AppError> {
    if let Some(parent) = Path::new(&path).parent() {
        fs::create_dir_all(parent).await?;
    }
    // create_new fails if anything (including a dangling symlink) is already there, so the
    // existence check and the write are one atomic step.
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .await
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(AppError::ToolValidation(format!(
                "File already exists: {path}. Use edit_file to overwrite."
            )));
        }
        Err(e) => return Err(e.into()),
    };
    file.write_all(content.as_bytes()).await?;
    file.flush().await?;
    Ok(())
}

pub async fn edit_file(path: String, content: String) -> Result<(), AppError> {
    if !fs::try_exists(&path).await? {
        return Err(AppError::ToolValidation(format!(
            "File does not exist: {path}. Use create_file to create a new file."
        )));
    }
    regular_file_metadata(&path).await?;
    fs::write(&path, content).await?;
    Ok(())
}

/// Delete the item at `path`. A symlink is removed as a link, never followed: its target
/// is left alone even when it is a directory.
pub async fn delete_file(path: String) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(&path).await?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        let removed = fs::remove_file(&path).await;
        // Windows directory symlinks/junctions are removed with remove_dir.
        #[cfg(windows)]
        let removed = match removed {
            Ok(()) => Ok(()),
            Err(_) => fs::remove_dir(&path).await,
        };
        removed?;
    } else if file_type.is_dir() {
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
        if !exists_nofollow(path).await {
            return Err(AppError::ToolValidation(format!(
                "Path does not exist: {path}"
            )));
        }
    }

    let total = paths.len();
    let mut deleted_paths: Vec<String> = Vec::new();
    for path in paths {
        // Already removed together with a directory listed earlier in the batch.
        if deleted_paths
            .iter()
            .any(|done| Path::new(&path).starts_with(done))
        {
            continue;
        }
        if let Err(e) = delete_file(path.clone()).await {
            return Err(AppError::ToolValidation(format!(
                "Deleted {} of {total}; failed on '{path}': {e}",
                deleted_paths.len()
            )));
        }
        deleted_paths.push(path);
    }
    Ok(total)
}

pub async fn rename_file(old_path: String, new_path: String) -> Result<(), AppError> {
    if !exists_nofollow(&old_path).await {
        return Err(AppError::ToolValidation(format!(
            "Source does not exist: {old_path}"
        )));
    }
    if exists_nofollow(&new_path).await {
        return Err(AppError::ToolValidation(format!(
            "Destination already exists: {new_path}"
        )));
    }
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
        if !exists_nofollow(old_path).await {
            return Err(AppError::ToolValidation(format!(
                "Source does not exist: {old_path}"
            )));
        }
        if exists_nofollow(new_path).await {
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

    let total = operations.len();
    for (done, (old_path, new_path)) in operations.into_iter().enumerate() {
        if let Err(e) = rename_file(old_path.clone(), new_path).await {
            return Err(AppError::ToolValidation(format!(
                "Renamed {done} of {total}; failed on '{old_path}': {e}"
            )));
        }
    }
    Ok(total)
}

pub async fn create_directory(path: String) -> Result<(), AppError> {
    if exists_nofollow(&path).await {
        return Err(AppError::ToolValidation(format!(
            "Path already exists: {path}"
        )));
    }
    fs::create_dir_all(&path).await?;
    Ok(())
}

pub async fn copy_file(source: String, destination: String) -> Result<(), AppError> {
    let source_meta = regular_file_metadata(&source).await?;
    if let Some(parent) = Path::new(&destination).parent() {
        fs::create_dir_all(parent).await?;
    }
    let mut dest = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .await
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(AppError::ToolValidation(format!(
                "Destination already exists: {destination}. Delete it first to overwrite."
            )));
        }
        Err(e) => return Err(e.into()),
    };

    let copied = async {
        let mut src = fs::File::open(&source).await?;
        tokio::io::copy(&mut src, &mut dest).await?;
        dest.flush().await?;
        fs::set_permissions(&destination, source_meta.permissions()).await
    }
    .await;
    if let Err(e) = copied {
        // Do not leave a truncated copy behind.
        let _ = fs::remove_file(&destination).await;
        return Err(e.into());
    }
    Ok(())
}

/// Shorten one result line to `max_chars` characters (char-boundary safe), noting the cut.
fn cap_line(line: &str, max_chars: usize) -> String {
    match line.char_indices().nth(max_chars) {
        None => line.to_owned(),
        Some((cut, _)) => {
            let extra = line[cut..].chars().count();
            format!("{}…(+{extra} chars)", &line[..cut])
        }
    }
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
            // DirEntry::file_type does not follow symlinks: links are never traversed or
            // read, so nothing outside the tree is reachable and link loops cannot occur.
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };

            if file_type.is_dir() {
                if !is_skipped_dir(&name) {
                    dirs_to_visit.push(path.to_string_lossy().into_owned());
                }
            } else if file_type.is_file() {
                // Skip files that exceed the read cap to avoid loading giant logs into RAM.
                let too_large = fs::metadata(&path)
                    .await
                    .map(|m| m.len() > MAX_READ_BYTES)
                    .unwrap_or(true);
                if too_large {
                    continue;
                }
                let Ok(bytes) = fs::read(&path).await else {
                    continue;
                };
                if bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
                    continue;
                }
                let Ok(content) = String::from_utf8(bytes) else {
                    continue;
                };
                let file_str = path.to_string_lossy();
                for (line_num, line) in content.lines().enumerate() {
                    if re.is_match(line) {
                        total_matches += 1;
                        if results.len() < max_results {
                            results.push(format!(
                                "{}:{}: {}",
                                file_str,
                                line_num + 1,
                                cap_line(line.trim(), MAX_SEARCH_LINE_CHARS)
                            ));
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

/// Regular files whose name matches `filename_regex`. Returns at most `max_matches` paths
/// (sorted) and whether more existed. Symlinks are not followed or matched.
pub async fn matching_files_capped(
    dir: String,
    filename_regex: String,
    recursive: bool,
    max_matches: usize,
) -> Result<(Vec<String>, bool), AppError> {
    let re = regex::Regex::new(&filename_regex)
        .map_err(|e| AppError::ToolValidation(format!("Invalid filename regex: {e}")))?;

    let mut matches = Vec::new();
    let mut truncated = false;
    let mut dirs_to_visit = vec![dir];
    let mut is_start_dir = true;

    'walk: while let Some(current_dir) = dirs_to_visit.pop() {
        // Only the directory the caller named must be readable; an unreadable subdirectory
        // is skipped rather than aborting the whole walk.
        let mut entries = match fs::read_dir(&current_dir).await {
            Ok(entries) => entries,
            Err(e) if is_start_dir => return Err(e.into()),
            Err(_) => continue,
        };
        is_start_dir = false;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };

            if file_type.is_dir() {
                if recursive && !is_skipped_dir(&name) {
                    dirs_to_visit.push(path.to_string_lossy().into_owned());
                }
            } else if file_type.is_file() && re.is_match(&name) {
                if matches.len() >= max_matches {
                    truncated = true;
                    break 'walk;
                }
                matches.push(path.to_string_lossy().into_owned());
            }
        }
    }

    matches.sort();
    Ok((matches, truncated))
}

/// Like `matching_files_capped`, but exceeding the cap is an error: bulk operations must
/// never act on a silently truncated set.
pub async fn matching_files(
    dir: String,
    filename_regex: String,
    recursive: bool,
    max_matches: usize,
) -> Result<Vec<String>, AppError> {
    let (matches, truncated) =
        matching_files_capped(dir, filename_regex, recursive, max_matches).await?;
    if truncated {
        return Err(AppError::ToolValidation(format!(
            "Matched more than {max_matches} files; narrow the pattern before running a bulk operation"
        )));
    }
    Ok(matches)
}

pub async fn patch_file(path: String, search: String, replace: String) -> Result<(), AppError> {
    regular_file_metadata(&path).await?;
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

#[cfg(test)]
pub(crate) mod test_util {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// Unique temp directory, removed on drop.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "foxmayn-test-{label}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::TempDir;
    use super::*;

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn create_file_refuses_existing_file() {
        let tmp = TempDir::new("create");
        let f = tmp.path().join("a.txt");
        create_file(s(&f), "one".into()).await.unwrap();
        assert!(create_file(s(&f), "two".into()).await.is_err());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "one");
    }

    #[tokio::test]
    async fn create_file_makes_parent_dirs() {
        let tmp = TempDir::new("create-parents");
        let f = tmp.path().join("x/y/z.txt");
        create_file(s(&f), "hi".into()).await.unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "hi");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn create_file_refuses_dangling_symlink() {
        let tmp = TempDir::new("create-dangling");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(tmp.path().join("target-missing"), &link).unwrap();
        assert!(create_file(s(&link), "x".into()).await.is_err());
        assert!(!tmp.path().join("target-missing").exists());
    }

    #[tokio::test]
    async fn copy_file_refuses_existing_destination() {
        let tmp = TempDir::new("copy");
        let src = tmp.path().join("src.txt");
        let dst = tmp.path().join("dst.txt");
        std::fs::write(&src, "new").unwrap();
        std::fs::write(&dst, "keep").unwrap();
        assert!(copy_file(s(&src), s(&dst)).await.is_err());
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "keep");

        let fresh = tmp.path().join("sub/fresh.txt");
        copy_file(s(&src), s(&fresh)).await.unwrap();
        assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "new");
    }

    #[tokio::test]
    async fn rename_file_refuses_existing_destination() {
        let tmp = TempDir::new("rename");
        let a = tmp.path().join("a.txt");
        let b = tmp.path().join("b.txt");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        assert!(rename_file(s(&a), s(&b)).await.is_err());
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "b");
        assert!(a.exists());

        let c = tmp.path().join("c.txt");
        rename_file(s(&a), s(&c)).await.unwrap();
        assert!(!a.exists() && c.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn delete_file_removes_symlink_not_target() {
        let tmp = TempDir::new("delete-link");
        let data = tmp.path().join("data");
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("keep.txt"), "x").unwrap();
        let link = tmp.path().join("cache");
        std::os::unix::fs::symlink(&data, &link).unwrap();

        delete_file(s(&link)).await.unwrap();
        assert!(std::fs::symlink_metadata(&link).is_err());
        assert!(data.join("keep.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rename_file_moves_symlink_not_target() {
        let tmp = TempDir::new("rename-link");
        let data = tmp.path().join("data");
        std::fs::create_dir(&data).unwrap();
        let link = tmp.path().join("cache");
        std::os::unix::fs::symlink(&data, &link).unwrap();
        let moved = tmp.path().join("cache2");

        rename_file(s(&link), s(&moved)).await.unwrap();
        assert!(data.is_dir());
        assert!(
            std::fs::symlink_metadata(&moved)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[tokio::test]
    async fn delete_many_prevalidates_all_paths() {
        let tmp = TempDir::new("delete-many");
        let a = tmp.path().join("a.txt");
        std::fs::write(&a, "a").unwrap();
        let missing = tmp.path().join("missing.txt");
        let err = delete_many(vec![s(&a), s(&missing)]).await.unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
        assert!(a.exists(), "nothing deleted when pre-validation fails");
    }

    #[tokio::test]
    async fn delete_many_tolerates_descendant_of_listed_directory() {
        let tmp = TempDir::new("delete-nested");
        let dir = tmp.path().join("d");
        std::fs::create_dir(&dir).unwrap();
        let child = dir.join("f.txt");
        std::fs::write(&child, "x").unwrap();
        let n = delete_many(vec![s(&dir), s(&child)]).await.unwrap();
        assert_eq!(n, 2);
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn rename_many_reports_partial_progress() {
        let tmp = TempDir::new("rename-many");
        let a = tmp.path().join("a.txt");
        std::fs::write(&a, "a").unwrap();
        let ok_dst = tmp.path().join("a2.txt");
        let b = tmp.path().join("b.txt");
        std::fs::write(&b, "b").unwrap();
        // Parent directory of the second destination does not exist: rename fails mid-batch.
        let bad_dst = tmp.path().join("nodir/b2.txt");
        let err = rename_many(vec![(s(&a), s(&ok_dst)), (s(&b), s(&bad_dst))])
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("Renamed 1 of 2; failed on"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn search_does_not_follow_symlinks() {
        let tmp = TempDir::new("search-links");
        let base = tmp.path().join("proj");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "TOPSECRET").unwrap();
        std::fs::write(base.join("ok.txt"), "TOPSECRET inside").unwrap();
        std::os::unix::fs::symlink(&outside, base.join("dirlink")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), base.join("filelink")).unwrap();
        std::os::unix::fs::symlink(&base, base.join("loop")).unwrap();

        let out = search_in_files(s(&base), "TOPSECRET".into(), 50)
            .await
            .unwrap();
        assert!(out.contains("ok.txt"), "{out}");
        assert!(!out.contains("secret.txt"), "{out}");
        assert!(!out.contains("dirlink") && !out.contains("filelink") && !out.contains("loop"));
        assert_eq!(out.matches("TOPSECRET").count(), 1);
    }

    #[tokio::test]
    async fn search_skips_binary_big_and_ignored_dirs_and_caps_lines() {
        let tmp = TempDir::new("search-filters");
        let base = tmp.path();
        std::fs::write(base.join("bin.dat"), b"needle\0binary").unwrap();
        std::fs::write(base.join("text.txt"), "needle here").unwrap();
        let mut big = vec![b'a'; MAX_READ_BYTES as usize + 1];
        big.extend_from_slice(b" needle");
        std::fs::write(base.join("big.log"), big).unwrap();
        for d in [
            ".git",
            "node_modules",
            ".venv",
            "venv",
            ".next",
            ".cache",
            "dist",
        ] {
            std::fs::create_dir(base.join(d)).unwrap();
            std::fs::write(base.join(d).join("x.txt"), "needle").unwrap();
        }
        // Multi-byte chars around the cap boundary must not panic.
        std::fs::write(
            base.join("long.txt"),
            format!("needle {}", "é".repeat(1000)),
        )
        .unwrap();

        let out = search_in_files(s(base), "needle".into(), 50).await.unwrap();
        assert!(out.contains("text.txt"), "{out}");
        assert!(out.contains("long.txt"), "{out}");
        assert!(
            !out.contains("bin.dat") && !out.contains("big.log"),
            "{out}"
        );
        for d in [
            ".git",
            "node_modules",
            ".venv",
            "venv",
            ".next",
            ".cache",
            "dist",
        ] {
            assert!(!out.contains(d), "{d} should be skipped: {out}");
        }
        let long_line = out.lines().find(|l| l.contains("long.txt")).unwrap();
        assert!(long_line.contains("…(+"), "{long_line}");
        assert!(long_line.chars().count() < base.to_string_lossy().chars().count() + 400);
    }

    #[test]
    fn cap_line_is_char_boundary_safe() {
        assert_eq!(cap_line("short", 300), "short");
        let capped = cap_line(&"é".repeat(10), 3);
        assert_eq!(capped, "ééé…(+7 chars)");
    }

    #[test]
    fn cap_chars_truncates_with_note() {
        assert_eq!(cap_chars("abc".into(), 5, "n"), "abc");
        let out = cap_chars("ééééé".into(), 2, "cut");
        assert!(out.starts_with("éé\n\n[cut]"), "{out}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn find_files_ignores_symlinks_and_caps_without_error() {
        let tmp = TempDir::new("find");
        let base = tmp.path().join("proj");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("o.txt"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, base.join("dirlink")).unwrap();
        std::os::unix::fs::symlink(&base, base.join("loop")).unwrap();
        for i in 0..5 {
            std::fs::write(base.join(format!("f{i}.txt")), "x").unwrap();
        }

        let (all, truncated) = matching_files_capped(s(&base), ".*".into(), true, 10)
            .await
            .unwrap();
        assert_eq!(all.len(), 5);
        assert!(!truncated);

        let (some, truncated) = matching_files_capped(s(&base), ".*".into(), true, 3)
            .await
            .unwrap();
        assert_eq!(some.len(), 3);
        assert!(truncated);

        // Exactly at the cap is not "more than the cap".
        assert!(matching_files(s(&base), ".*".into(), true, 5).await.is_ok());
        assert!(
            matching_files(s(&base), ".*".into(), true, 4)
                .await
                .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reads_refuse_non_regular_files() {
        let tmp = TempDir::new("fifo");
        let fifo = tmp.path().join("pipe");
        let status = std::process::Command::new("mkfifo").arg(&fifo).status();
        if !matches!(status, Ok(st) if st.success()) {
            return; // mkfifo not available
        }
        assert!(read_file(s(&fifo)).await.is_err());
        assert!(read_pdf(s(&fifo)).await.is_err());
        assert!(edit_file(s(&fifo), "x".into()).await.is_err());
        assert!(patch_file(s(&fifo), "a".into(), "b".into()).await.is_err());
        assert!(
            copy_file(s(&fifo), s(&tmp.path().join("copy")))
                .await
                .is_err()
        );
        // search/find skip it rather than hanging
        let out = search_in_files(s(tmp.path()), ".".into(), 5).await.unwrap();
        assert_eq!(out, "No matches found.");
        let (found, _) = matching_files_capped(s(tmp.path()), ".*".into(), true, 5)
            .await
            .unwrap();
        assert!(found.is_empty());
    }

    #[tokio::test]
    async fn list_files_lists_sorted_dirs_first() {
        let tmp = TempDir::new("list");
        std::fs::write(tmp.path().join("b.txt"), "x").unwrap();
        std::fs::create_dir(tmp.path().join("zdir")).unwrap();
        let entries = list_files(s(tmp.path())).await.unwrap();
        assert_eq!(entries[0].name, "zdir");
        assert_eq!(entries[1].name, "b.txt");
    }
}
