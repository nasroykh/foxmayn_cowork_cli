use crate::llm::types::{Tool, ToolFunction};

pub fn tool_definitions() -> Vec<Tool> {
    vec![
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "list_files".into(),
                description: "Read-only. List the immediate contents (files and directories) of a single directory. NOT recursive — to find files in subdirectories or by extension, use `find_files` instead.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Required. Relative path of the directory to list. Use '.' for the working directory root, or a subpath like 'src' or 'src/utils'. Never leave this empty." }
                    },
                    "required": ["path"]
                }),
            },
        },
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "read_file".into(),
                description: "Read-only. Return the full text contents of a single file.".into(),
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
                name: "read_pdf".into(),
                description: "Read-only. Extract and return the text content of a PDF file.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the PDF file. Prefer relative paths (e.g. 'docs/report.pdf'). Relative paths are resolved against the working directory." }
                    },
                    "required": ["path"]
                }),
            },
        },
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "create_file".into(),
                description: "Create a NEW file with the given content. Fails if the file already exists. Use ONLY when the user has asked to create a file."
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
                description: "Overwrite an existing file's ENTIRE content. Destructive — discards everything currently in the file. Prefer `patch_file` for small targeted changes; only use `edit_file` when the user explicitly asks to rewrite the whole file or when `patch_file` cannot be used. Use ONLY when the user has asked to modify this file.".into(),
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
                description: "Delete a single file or directory. Destructive and irreversible. Use ONLY when the user has explicitly asked to delete this exact path. Never call to 'clean up', 'tidy', or remove files the user did not name.".into(),
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
                description: "Delete multiple explicit files or directories in one confirmed batch. Destructive and irreversible. Use ONLY when the user has explicitly listed several paths to delete; prefer this over repeated `delete_file` calls in that case.".into(),
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
                description: "Rename or move a single file or directory. Use ONLY when the user has explicitly asked to rename or move this path.".into(),
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
                description: "Rename or move multiple explicit files/directories in one confirmed batch. Fails before changing anything if a destination already exists or is duplicated. Use ONLY when the user has explicitly listed several rename/move operations.".into(),
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
                description: "Delete files whose filename matches a regex under a directory, in one confirmed batch. Destructive and irreversible. Files only; directories are not deleted. Use ONLY when the user has explicitly asked to delete files by pattern (e.g. the user said 'delete all .tmp files'). NEVER use to enumerate, count, or analyze files — for that use `find_files`.".into(),
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
                description: "Bulk rename files whose filename matches a regex under a directory. The replacement is applied to filenames only, not full paths. Fails before changing anything if a destination exists or is duplicated. Use ONLY when the user has explicitly asked to rename files by pattern.".into(),
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
                description: "Create a new directory. Parent directories are created as needed. Fails if the path already exists. Use ONLY when the user has asked to create a directory.".into(),
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
                description: "Copy a single file to a new location. Fails if the destination already exists. Use ONLY when the user has asked to copy a file.".into(),
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
                name: "find_files".into(),
                description: "Read-only. Find files under a directory, optionally filtered by a filename regex. Returns matching file paths (capped at 200). Use this whenever you need to list, count, enumerate, or categorize files. To get EVERY file (e.g. for a filetype breakdown of the whole repo), call with `path: '.'`, `recursive: true`, and OMIT `filename_regex` (it defaults to matching all files). Skips hidden directories and build artifacts.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Required. Relative directory to search under. Use '.' to scan the entire working directory, or a subpath like 'src'." },
                        "filename_regex": { "type": "string", "description": "Optional. Regex applied to each filename (not the full path), e.g. '\\\\.md$' for .md files or '^README' for README files. Omit this argument entirely to match ALL files." },
                        "recursive": { "type": "boolean", "description": "Whether to scan subdirectories. Set to true to search the whole tree (use this when the user asks about the 'repo', 'project', or 'all files'). Defaults to false." }
                    },
                    "required": ["path"]
                }),
            },
        },
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "search_in_files".into(),
                description: "Read-only. Search for a regex pattern inside the CONTENTS of files under a directory (grep-style). Returns matching lines with file paths and line numbers. For finding files by NAME or extension, use `find_files` instead. Skips hidden directories and build artifacts.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Required. Relative directory to search under. Use '.' to scan the entire working directory, or a subpath like 'src'." },
                        "pattern": { "type": "string", "description": "Regex pattern to search for in file contents (e.g. 'fn main', 'TODO', 'error\\(')" }
                    },
                    "required": ["path", "pattern"]
                }),
            },
        },
        Tool {
            r#type: "function".into(),
            function: ToolFunction {
                name: "patch_file".into(),
                description: "Replace a specific text occurrence inside an existing file. The `search` text must appear EXACTLY ONCE in the file — fails on 0 or multiple matches. Preferred over `edit_file` for small targeted changes. Use ONLY when the user has asked to modify this file.".into(),
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
