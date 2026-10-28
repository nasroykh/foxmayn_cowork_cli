use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::AppError;
use crate::llm::types::FunctionCall;

use super::descriptions::{build_confirmation_description, build_description, hidden_write_target};
use super::validate::{
    extract_bool, extract_rename_operations, extract_str, extract_str_array, resolve_paths,
    resolve_rename_operations, validate_generated_filename, validate_path_containment,
    validate_removal_path, validate_target_path,
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
        validate_target_path(&new_path, base_path)?;
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
    // Creating something under a hidden path (.env, .cargo/config.toml, .github/...) can
    // change how tools behave later, so it is confirmed like a destructive op.
    let needs_confirmation = DESTRUCTIVE_OPS.contains(&call.name.as_str())
        || hidden_write_target(call, base_path)?.is_some();

    if needs_confirmation {
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
            let resolved = validate_target_path(&path, base_path)?
                .to_string_lossy()
                .into_owned();
            crate::fs::create_file(resolved, content).await?;
            Ok("File created successfully".into())
        }
        "edit_file" => {
            let path = extract_str(args, "path")?;
            let content = extract_str(args, "content")?;
            let resolved = validate_target_path(&path, base_path)?
                .to_string_lossy()
                .into_owned();
            crate::fs::edit_file(resolved, content).await?;
            Ok("File updated successfully".into())
        }
        "delete_file" => {
            let path = extract_str(args, "path")?;
            let resolved = validate_removal_path(&path, base_path)?
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
            let resolved_old = validate_removal_path(&old_path, base_path)?
                .to_string_lossy()
                .into_owned();
            let resolved_new = validate_target_path(&new_path, base_path)?
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
            let resolved = validate_target_path(&path, base_path)?
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
            let resolved_dst = validate_target_path(&destination, base_path)?
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
            let (matches, truncated) = crate::fs::matching_files_capped(
                resolved,
                filename_regex,
                recursive,
                MAX_BULK_MATCHES,
            )
            .await?;
            if matches.is_empty() {
                Ok("No matching files found.".into())
            } else if truncated {
                Ok(format!(
                    "{}\n\n(showing {MAX_BULK_MATCHES} matches; more exist, narrow the pattern or directory)",
                    matches.join("\n")
                ))
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
            let resolved = validate_target_path(&path, base_path)?
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::test_util::TempDir;
    use serde_json::json;

    fn call(name: &str, arguments: serde_json::Value) -> FunctionCall {
        FunctionCall {
            name: name.into(),
            arguments,
        }
    }

    fn project() -> (TempDir, PathBuf) {
        let tmp = TempDir::new("dispatch");
        let base = tmp.path().join("proj");
        std::fs::create_dir_all(&base).unwrap();
        (tmp, base)
    }

    #[tokio::test]
    async fn plain_create_runs_without_confirmation() {
        let (_tmp, base) = project();
        let r = dispatch_tool_call(
            &call(
                "create_file",
                json!({"path": "notes/a.txt", "content": "hi"}),
            ),
            &base,
        )
        .await
        .unwrap();
        assert!(!r.requires_confirmation);
        assert!(base.join("notes/a.txt").exists());
    }

    #[tokio::test]
    async fn hidden_writes_require_confirmation_and_do_not_execute() {
        let (_tmp, base) = project();
        let cases = [
            (
                "create_file",
                json!({"path": ".env", "content": "A=1"}),
                ".env",
            ),
            (
                "create_file",
                json!({"path": ".cargo/config.toml", "content": "x"}),
                ".cargo",
            ),
            (
                "create_file",
                json!({"path": ".github/workflows/x.yml", "content": "x"}),
                ".github",
            ),
            (
                "create_file",
                json!({"path": "src/.hidden", "content": "x"}),
                ".hidden",
            ),
            ("create_directory", json!({"path": ".vscode"}), ".vscode"),
        ];
        for (name, args, needle) in cases {
            let r = dispatch_tool_call(&call(name, args), &base).await.unwrap();
            assert!(r.requires_confirmation, "{name} {needle}");
            assert!(r.result.is_none());
            assert!(r.description.contains("hidden"), "{}", r.description);
            assert!(r.description.contains(needle), "{}", r.description);
        }
        assert!(std::fs::read_dir(&base).unwrap().next().is_none());

        std::fs::write(base.join("a.txt"), "x").unwrap();
        let r = dispatch_tool_call(
            &call(
                "copy_file",
                json!({"source": "a.txt", "destination": ".backup/a.txt"}),
            ),
            &base,
        )
        .await
        .unwrap();
        assert!(r.requires_confirmation);
        assert!(r.description.contains(".backup"), "{}", r.description);
        assert!(!base.join(".backup").exists());

        // After approval, the same arguments execute through execute_tool.
        let approved = execute_tool(
            &call("create_file", json!({"path": ".env", "content": "A=1"})),
            &base,
        )
        .await
        .unwrap();
        assert_eq!(approved, "File created successfully");
        assert_eq!(std::fs::read_to_string(base.join(".env")).unwrap(), "A=1");
    }

    #[tokio::test]
    async fn destructive_ops_on_base_are_rejected() {
        let (_tmp, base) = project();
        let abs = base.to_string_lossy().into_owned();
        for p in ["", ".", "./", abs.as_str()] {
            for (name, args) in [
                ("delete_file", json!({"path": p})),
                ("delete_many", json!({"paths": [p]})),
                ("rename_file", json!({"old_path": p, "new_path": "x"})),
                ("rename_file", json!({"old_path": "a", "new_path": p})),
                ("edit_file", json!({"path": p, "content": "x"})),
                (
                    "patch_file",
                    json!({"path": p, "search": "a", "replace": "b"}),
                ),
                (
                    "rename_many",
                    json!({"operations": [{"old_path": p, "new_path": "x"}]}),
                ),
            ] {
                let c = call(name, args);
                assert!(dispatch_tool_call(&c, &base).await.is_err(), "{name} {p:?}");
                assert!(execute_tool(&c, &base).await.is_err(), "{name} {p:?}");
            }
        }
        assert!(base.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn create_through_symlinked_parent_is_rejected() {
        let (tmp, base) = project();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, base.join("docs")).unwrap();
        std::os::unix::fs::symlink(outside.join("nope"), base.join("dangling")).unwrap();
        std::fs::write(base.join("src.txt"), "x").unwrap();

        for (name, args) in [
            (
                "create_file",
                json!({"path": "docs/pwned.txt", "content": "x"}),
            ),
            ("create_file", json!({"path": "dangling", "content": "x"})),
            ("create_directory", json!({"path": "docs/sub"})),
            (
                "copy_file",
                json!({"source": "src.txt", "destination": "docs/c.txt"}),
            ),
            (
                "rename_file",
                json!({"old_path": "src.txt", "new_path": "docs/r.txt"}),
            ),
        ] {
            let c = call(name, args);
            assert!(execute_tool(&c, &base).await.is_err(), "{name}");
        }
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
        assert!(base.join("src.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn delete_and_rename_act_on_symlink_itself() {
        let (_tmp, base) = project();
        std::fs::create_dir(base.join("data")).unwrap();
        std::fs::write(base.join("data/keep.txt"), "x").unwrap();
        std::os::unix::fs::symlink(base.join("data"), base.join("cache")).unwrap();

        execute_tool(
            &call(
                "rename_file",
                json!({"old_path": "cache", "new_path": "cache2"}),
            ),
            &base,
        )
        .await
        .unwrap();
        assert!(base.join("data/keep.txt").exists());
        assert!(std::fs::symlink_metadata(base.join("cache2")).is_ok());

        execute_tool(&call("delete_file", json!({"path": "cache2"})), &base)
            .await
            .unwrap();
        assert!(std::fs::symlink_metadata(base.join("cache2")).is_err());
        assert!(base.join("data/keep.txt").exists());
    }

    #[tokio::test]
    async fn single_rename_and_create_and_copy_refuse_existing_destination() {
        let (_tmp, base) = project();
        std::fs::write(base.join("a.txt"), "a").unwrap();
        std::fs::write(base.join("b.txt"), "b").unwrap();
        for (name, args) in [
            (
                "rename_file",
                json!({"old_path": "a.txt", "new_path": "b.txt"}),
            ),
            ("create_file", json!({"path": "b.txt", "content": "z"})),
            (
                "copy_file",
                json!({"source": "a.txt", "destination": "b.txt"}),
            ),
        ] {
            assert!(
                execute_tool(&call(name, args), &base).await.is_err(),
                "{name}"
            );
        }
        assert_eq!(std::fs::read_to_string(base.join("b.txt")).unwrap(), "b");
    }

    #[tokio::test]
    async fn confirmation_descriptions_are_informative() {
        let (_tmp, base) = project();
        std::fs::write(base.join("f.txt"), "old").unwrap();
        let content = (1..=40).map(|i| format!("line {i}\n")).collect::<String>();
        let edit = dispatch_tool_call(
            &call("edit_file", json!({"path": "f.txt", "content": content})),
            &base,
        )
        .await
        .unwrap();
        assert!(edit.requires_confirmation);
        assert!(edit.description.contains("f.txt"));
        assert!(edit.description.contains("line 1"));
        assert!(!edit.description.contains("line 11"));
        assert!(
            edit.description.contains("40 line(s)"),
            "{}",
            edit.description
        );
        assert!(edit.description.contains("more chars)"));

        let patch = dispatch_tool_call(
            &call(
                "patch_file",
                json!({"path": "f.txt", "search": "old", "replace": "n".repeat(900)}),
            ),
            &base,
        )
        .await
        .unwrap();
        assert!(
            patch.description.contains("(300 more chars)"),
            "{}",
            patch.description
        );

        let delete = dispatch_tool_call(&call("delete_file", json!({"path": "./f.txt"})), &base)
            .await
            .unwrap();
        assert_eq!(delete.description, "Delete f.txt");

        for i in 0..20 {
            std::fs::write(base.join(format!("x{i}.log")), "x").unwrap();
        }
        let bulk = dispatch_tool_call(
            &call(
                "delete_matching",
                json!({"path": ".", "filename_regex": "\\.log$"}),
            ),
            &base,
        )
        .await
        .unwrap();
        assert!(
            bulk.description.contains("Delete 20 file(s)"),
            "{}",
            bulk.description
        );
        assert!(
            bulk.description.contains("(20 total)"),
            "{}",
            bulk.description
        );
        assert!(bulk.description.contains("under ."), "{}", bulk.description);
    }

    #[tokio::test]
    async fn find_files_caps_results_with_a_note() {
        let (_tmp, base) = project();
        for i in 0..(MAX_BULK_MATCHES + 5) {
            std::fs::write(base.join(format!("f{i}.txt")), "x").unwrap();
        }
        let out = execute_tool(
            &call("find_files", json!({"path": ".", "recursive": true})),
            &base,
        )
        .await
        .unwrap();
        assert!(out.contains("more exist"), "{out}");
        assert_eq!(
            out.lines().filter(|l| l.ends_with(".txt")).count(),
            MAX_BULK_MATCHES
        );
    }

    #[tokio::test]
    async fn bad_arguments_return_errors_instead_of_panicking() {
        let (_tmp, base) = project();
        for (name, args) in [
            ("read_file", json!({})),
            ("read_file", json!({"path": 5})),
            ("create_file", json!({"path": "\u{0}bad", "content": "x"})),
            ("delete_many", json!({"paths": "nope"})),
            ("delete_many", json!({"paths": []})),
            ("rename_many", json!({"operations": [{"old_path": 1}]})),
            ("search_in_files", json!({"path": ".", "pattern": "("})),
            (
                "rename_matching",
                json!({"path": ".", "filename_regex": "a", "replacement": "/"}),
            ),
            ("find_files", json!({"path": "é/ü/日本"})),
        ] {
            let c = call(name, args);
            let _ = execute_tool(&c, &base).await;
            let _ = dispatch_tool_call(&c, &base).await;
        }
    }
}
