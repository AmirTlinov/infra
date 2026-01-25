use crate::errors::ToolError;
use std::path::{Path, PathBuf};

fn ensure_inside_root(root: &Path, candidate: &Path) -> Result<(), ToolError> {
    if candidate == root {
        return Ok(());
    }
    if !candidate.starts_with(root) {
        return Err(ToolError::denied("Path escapes sandbox root")
            .with_hint("Use a path inside repo_root (sandbox root)."));
    }
    Ok(())
}

pub fn resolve_sandbox_path(
    root_dir: &Path,
    candidate: Option<&Path>,
    must_exist: bool,
) -> Result<PathBuf, ToolError> {
    if root_dir.as_os_str().is_empty() {
        return Err(ToolError::invalid_params(
            "rootDir must be a non-empty string",
        ));
    }
    let root_real = std::fs::canonicalize(root_dir)
        .map_err(|_| ToolError::invalid_params("rootDir must be a valid path"))?;

    if candidate.is_none() {
        return Ok(root_real);
    }
    let candidate = candidate.unwrap();
    let resolved = root_real.join(candidate);
    ensure_inside_root(&root_real, &resolved)?;

    if must_exist {
        let real = std::fs::canonicalize(&resolved)
            .map_err(|_| ToolError::invalid_params("path must exist"))?;
        ensure_inside_root(&root_real, &real)?;
        return Ok(real);
    }

    let parent = resolved
        .parent()
        .ok_or_else(|| ToolError::invalid_params("path must have parent"))?;
    let parent_real = std::fs::canonicalize(parent)
        .map_err(|_| ToolError::invalid_params("path must have valid parent"))?;
    ensure_inside_root(&root_real, &parent_real)?;
    let final_path = parent_real.join(
        resolved
            .file_name()
            .ok_or_else(|| ToolError::invalid_params("path must have filename"))?,
    );
    ensure_inside_root(&root_real, &final_path)?;
    Ok(final_path)
}
