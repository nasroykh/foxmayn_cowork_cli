use std::path::{Path, PathBuf};

use crate::error::AppError;

pub fn extract_str(args: &serde_json::Value, key: &str) -> Result<String, AppError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| AppError::ToolValidation(format!("Missing required argument '{key}'")))
}

pub fn extract_bool(args: &serde_json::Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

pub fn extract_str_array(args: &serde_json::Value, key: &str) -> Result<Vec<String>, AppError> {
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

pub fn validate_path_containment(path: &str, base_path: &Path) -> Result<PathBuf, AppError> {
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

pub fn resolve_paths(paths: Vec<String>, base_path: &Path) -> Result<Vec<String>, AppError> {
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

pub fn extract_rename_operations(
    args: &serde_json::Value,
) -> Result<Vec<(String, String)>, AppError> {
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

pub fn resolve_rename_operations(
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

pub fn validate_generated_filename(name: &str) -> Result<(), AppError> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(AppError::ToolValidation(format!(
            "Bulk rename generated an invalid filename: {name:?}"
        )));
    }
    Ok(())
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
