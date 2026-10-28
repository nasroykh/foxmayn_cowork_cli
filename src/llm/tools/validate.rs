use std::path::{Component, Path, PathBuf};

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

/// Canonical form of the working directory. Every comparison below canonicalizes both
/// sides the same way, so Windows `\\?\` verbatim prefixes line up in `starts_with`.
pub fn canonical_base(base_path: &Path) -> Result<PathBuf, AppError> {
    base_path
        .canonicalize()
        .map_err(|_| AppError::ToolValidation("Working directory not accessible".into()))
}

/// Join `path` onto the canonical base (or take it as-is when absolute), rejecting `..`.
/// Relative paths are pushed component by component so no `/` is left inside a Windows
/// verbatim (`\\?\`) path, where it would be a literal character.
fn lexical_target(path: &str, canonical_base: &Path) -> Result<PathBuf, AppError> {
    let target = Path::new(path);

    if target.components().any(|c| c == Component::ParentDir) {
        return Err(AppError::ToolValidation(format!(
            "Path '{path}' contains '..' components which are not allowed"
        )));
    }

    if target.is_absolute() {
        return Ok(target.to_path_buf());
    }

    let mut joined = canonical_base.to_path_buf();
    for component in target.components() {
        match component {
            Component::Normal(name) => joined.push(name),
            Component::CurDir => {}
            _ => {
                return Err(AppError::ToolValidation(format!(
                    "Path '{path}' must be relative to the working directory or absolute inside it"
                )));
            }
        }
    }
    Ok(joined)
}

/// Canonicalize the deepest existing ancestor of `path` and re-append the components
/// that do not exist yet. Components that do not exist cannot be symlinks, so the result
/// is where a write would really land. A dangling symlink is an error: creating through
/// it would write to wherever it points.
fn canonicalize_lenient(path: &Path) -> Result<PathBuf, AppError> {
    let unresolvable =
        || AppError::ToolValidation(format!("Path '{}' cannot be resolved", path.display()));
    let mut existing = path.to_path_buf();
    let mut missing: Vec<std::ffi::OsString> = Vec::new();

    let mut resolved = loop {
        match existing.canonicalize() {
            Ok(canonical) => break canonical,
            Err(_) => {
                if std::fs::symlink_metadata(&existing).is_ok() {
                    return Err(AppError::ToolValidation(format!(
                        "Path '{}' goes through a broken or unreadable symlink",
                        path.display()
                    )));
                }
                let name = existing
                    .file_name()
                    .map(ToOwned::to_owned)
                    .ok_or_else(unresolvable)?;
                missing.push(name);
                if !existing.pop() {
                    return Err(unresolvable());
                }
            }
        }
    };

    for name in missing.into_iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

fn ensure_inside(resolved: &Path, canonical_base: &Path, original: &str) -> Result<(), AppError> {
    if resolved.starts_with(canonical_base) {
        Ok(())
    } else {
        Err(AppError::ToolValidation(format!(
            "Path '{original}' is outside the working directory"
        )))
    }
}

fn base_itself_error(path: &str) -> AppError {
    AppError::ToolValidation(format!(
        "Path '{path}' is the working directory itself, which cannot be modified, deleted, or renamed"
    ))
}

/// Resolve `path` to the real location it points at (symlinks followed) and require it to
/// be inside the working directory. The path itself may not exist yet.
pub fn validate_path_containment(path: &str, base_path: &Path) -> Result<PathBuf, AppError> {
    let base = canonical_base(base_path)?;
    let resolved = canonicalize_lenient(&lexical_target(path, &base)?)?;
    ensure_inside(&resolved, &base, path)?;
    Ok(resolved)
}

/// Like `validate_path_containment`, but the target must not be the working directory
/// itself. Used for every tool that writes, creates, or renames onto a path.
pub fn validate_target_path(path: &str, base_path: &Path) -> Result<PathBuf, AppError> {
    let base = canonical_base(base_path)?;
    let resolved = validate_path_containment(path, base_path)?;
    if resolved == base {
        return Err(base_itself_error(path));
    }
    Ok(resolved)
}

/// For delete and the source of a rename: if the final component is a symlink the result
/// names the link itself (its parent is resolved and checked), so the operation never
/// touches whatever the link points at. Rejects the working directory itself.
pub fn validate_removal_path(path: &str, base_path: &Path) -> Result<PathBuf, AppError> {
    let base = canonical_base(base_path)?;
    let lexical = lexical_target(path, &base)?;

    let is_link = std::fs::symlink_metadata(&lexical)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);

    if is_link {
        let (Some(parent), Some(name)) = (lexical.parent(), lexical.file_name()) else {
            return Err(AppError::ToolValidation(format!(
                "Path '{path}' cannot be resolved"
            )));
        };
        let resolved_parent = canonicalize_lenient(parent)?;
        ensure_inside(&resolved_parent, &base, path)?;
        return Ok(resolved_parent.join(name));
    }

    let resolved = canonicalize_lenient(&lexical)?;
    ensure_inside(&resolved, &base, path)?;
    if resolved == base {
        return Err(base_itself_error(path));
    }
    Ok(resolved)
}

/// Path relative to the working directory, for dialogs and messages. Falls back to the
/// input when it is not under the base.
pub fn relative_display(path: &Path, canonical_base: &Path) -> String {
    match path.strip_prefix(canonical_base) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// `Some(relative path)` when the (validated) target sits under a hidden file or
/// directory, i.e. any component relative to the base starts with `.`.
pub fn hidden_relative_path(path: &str, base_path: &Path) -> Result<Option<String>, AppError> {
    let base = canonical_base(base_path)?;
    let resolved = validate_target_path(path, base_path)?;
    let Ok(rel) = resolved.strip_prefix(&base) else {
        return Ok(None);
    };
    let hidden = rel
        .components()
        .any(|c| matches!(c, Component::Normal(n) if n.to_string_lossy().starts_with('.')));
    Ok(hidden.then(|| rel.to_string_lossy().into_owned()))
}

pub fn resolve_paths(paths: Vec<String>, base_path: &Path) -> Result<Vec<String>, AppError> {
    if paths.is_empty() {
        return Err(AppError::ToolValidation("No paths were provided".into()));
    }

    paths
        .iter()
        .map(|path| {
            validate_removal_path(path, base_path).map(|p| p.to_string_lossy().into_owned())
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
            let old_resolved = validate_removal_path(old_path, base_path)?
                .to_string_lossy()
                .into_owned();
            let new_resolved = validate_target_path(new_path, base_path)?
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

#[cfg(test)]
mod sandbox_tests {
    use super::*;
    use crate::fs::test_util::TempDir;

    /// `<tmp>/proj` is the working directory, `<tmp>/outside` is a sibling it must not reach.
    fn setup() -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new("validate");
        let base = tmp.path().join("proj");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        (tmp, base, outside)
    }

    #[test]
    fn rejects_base_itself_for_mutating_targets() {
        let (_tmp, base, _) = setup();
        let abs = base.to_string_lossy().into_owned();
        for p in ["", ".", "./", "./.", abs.as_str()] {
            assert!(validate_target_path(p, &base).is_err(), "target {p:?}");
            assert!(validate_removal_path(p, &base).is_err(), "removal {p:?}");
            // plain containment (reads) still allows the base
            assert!(validate_path_containment(p, &base).is_ok(), "read {p:?}");
        }
    }

    #[test]
    fn rejects_parent_dir_in_the_middle() {
        let (_tmp, base, _) = setup();
        assert!(validate_path_containment("a/../b", &base).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_new_file_under_symlinked_parent() {
        let (_tmp, base, outside) = setup();
        std::os::unix::fs::symlink(&outside, base.join("docs")).unwrap();
        assert!(validate_path_containment("docs/pwned.txt", &base).is_err());
        assert!(validate_path_containment("docs/new/deeper/pwned.txt", &base).is_err());
        assert!(validate_target_path("docs/pwned.txt", &base).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_dangling_symlink_inside_path() {
        let (_tmp, base, outside) = setup();
        std::os::unix::fs::symlink(outside.join("nope"), base.join("dangling")).unwrap();
        assert!(validate_path_containment("dangling", &base).is_err());
        assert!(validate_path_containment("dangling/x.txt", &base).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn allows_new_file_under_in_tree_symlink() {
        let (_tmp, base, _) = setup();
        std::fs::create_dir(base.join("real")).unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("alias")).unwrap();
        let resolved = validate_path_containment("alias/new.txt", &base).unwrap();
        assert!(resolved.starts_with(base.canonicalize().unwrap().join("real")));
    }

    #[cfg(unix)]
    #[test]
    fn removal_path_keeps_in_tree_symlink_unresolved() {
        let (_tmp, base, _) = setup();
        std::fs::create_dir(base.join("data")).unwrap();
        std::os::unix::fs::symlink(base.join("data"), base.join("cache")).unwrap();
        let cbase = base.canonicalize().unwrap();
        assert_eq!(
            validate_removal_path("cache", &base).unwrap(),
            cbase.join("cache")
        );
        // The follow-variant resolves to the target instead.
        assert_eq!(
            validate_path_containment("cache", &base).unwrap(),
            cbase.join("data")
        );
    }

    #[cfg(unix)]
    #[test]
    fn removal_path_allows_outward_link_itself_but_not_below_it() {
        let (_tmp, base, outside) = setup();
        std::os::unix::fs::symlink(&outside, base.join("out")).unwrap();
        assert!(validate_removal_path("out", &base).is_ok());
        assert!(validate_removal_path("out/secret.txt", &base).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn removal_path_accepts_link_pointing_at_base() {
        let (_tmp, base, _) = setup();
        std::os::unix::fs::symlink(&base, base.join("loop")).unwrap();
        assert!(validate_removal_path("loop", &base).is_ok());
    }

    #[test]
    fn rejects_absolute_new_path_outside_base() {
        let (_tmp, base, outside) = setup();
        let p = outside.join("x.txt");
        assert!(validate_path_containment(&p.to_string_lossy(), &base).is_err());
    }

    #[test]
    fn allows_absolute_new_path_inside_base() {
        let (_tmp, base, _) = setup();
        let p = base.join("sub").join("x.txt");
        let resolved = validate_target_path(&p.to_string_lossy(), &base).unwrap();
        assert_eq!(
            resolved,
            base.canonicalize().unwrap().join("sub").join("x.txt")
        );
    }

    #[test]
    fn hidden_relative_path_detects_dot_components() {
        let (_tmp, base, _) = setup();
        let hidden = |p: &str| hidden_relative_path(p, &base).unwrap();
        assert!(hidden(".env").is_some());
        assert!(hidden(".cargo/config.toml").is_some());
        assert!(hidden(".github/workflows/x.yml").is_some());
        assert!(hidden("src/.hidden").is_some());
        assert!(hidden("./.vscode/tasks.json").is_some());
        assert!(hidden("notes.txt").is_none());
        assert!(hidden("src/lib.rs").is_none());
        assert!(hidden("a.b/c.d").is_none());
        assert!(hidden("./notes.txt").is_none());
    }

    #[test]
    fn hidden_relative_path_ignores_hidden_base() {
        let tmp = TempDir::new("hidden-base");
        let base = tmp.path().join(".workdir");
        std::fs::create_dir_all(&base).unwrap();
        assert!(hidden_relative_path("notes.txt", &base).unwrap().is_none());
    }

    #[test]
    fn relative_display_is_base_relative() {
        let base = Path::new("proj");
        assert_eq!(relative_display(&base.join("a.txt"), base), "a.txt");
        assert_eq!(relative_display(base, base), ".");
    }
}
