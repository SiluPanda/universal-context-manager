use anyhow::{Context, Result, anyhow};
use context_core::{ScopeKind, ScopeRef};
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn current_directory() -> Result<PathBuf> {
    std::env::current_dir().context("resolve current directory")
}

pub(crate) fn resolve_project_scope_id(supplied: Option<String>, cwd: &Path) -> Result<String> {
    match supplied {
        Some(value) if !value.trim().is_empty() => {
            let trimmed = value.trim();
            let path = PathBuf::from(trimmed);
            let candidate = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            if candidate.is_dir() {
                Ok(resolve_project_directory(Some(trimmed), cwd)?
                    .display()
                    .to_string())
            } else {
                Ok(value)
            }
        }
        _ => Ok(default_project_directory(cwd)?.display().to_string()),
    }
}

pub(crate) fn resolve_scope(
    kind: ScopeKind,
    supplied_id: Option<String>,
    cwd: &Path,
) -> Result<ScopeRef> {
    match kind {
        ScopeKind::Global => Ok(ScopeRef::global()),
        ScopeKind::Project => ScopeRef::normalized(
            ScopeKind::Project,
            resolve_project_scope_id(supplied_id, cwd)?,
        )
        .map_err(Into::into),
        ScopeKind::Task => {
            let id = supplied_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    anyhow!("task scope requires --scope-id; use a stable task or issue identifier")
                })?;
            ScopeRef::normalized(ScopeKind::Task, id).map_err(Into::into)
        }
    }
}

pub(crate) fn resolve_project_directory(supplied: Option<&str>, cwd: &Path) -> Result<PathBuf> {
    let selected = match supplied {
        Some(value) if !value.trim().is_empty() => {
            let path = PathBuf::from(value.trim());
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        }
        _ => return default_project_directory(cwd),
    };
    let selected = selected
        .canonicalize()
        .with_context(|| format!("resolve project directory {}", selected.display()))?;
    if !selected.is_dir() {
        return Err(anyhow!(
            "project path is not a directory: {}",
            selected.display()
        ));
    }
    Ok(discover_git_root(&selected).unwrap_or(selected))
}

pub(crate) fn default_project_directory(cwd: &Path) -> Result<PathBuf> {
    let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    if let Some(root) = discover_git_root(&canonical) {
        return Ok(root);
    }
    Ok(canonical)
}

fn discover_git_root(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8(output.stdout).ok()?;
    let root = root.trim();
    if root.is_empty() {
        None
    } else {
        let root = PathBuf::from(root);
        Some(root.canonicalize().unwrap_or(root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn defaults_project_scope_to_git_root() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().join("repository");
        let nested = root.join("a/b");
        std::fs::create_dir_all(&nested).expect("mkdir");
        let status = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "--quiet"])
            .status()
            .expect("git init");
        assert!(status.success());

        let resolved = resolve_project_scope_id(None, &nested).expect("scope");
        assert_eq!(
            PathBuf::from(resolved).canonicalize().expect("canonical"),
            root.canonicalize().expect("root canonical")
        );
    }

    #[test]
    fn project_scope_falls_back_to_current_directory() {
        let dir = tempdir().expect("tempdir");
        let resolved = resolve_project_scope_id(None, dir.path()).expect("scope");
        assert_eq!(
            PathBuf::from(resolved).canonicalize().expect("canonical"),
            dir.path().canonicalize().expect("root canonical")
        );
    }

    #[test]
    fn task_scope_never_guesses_an_identifier() {
        let dir = tempdir().expect("tempdir");
        let error = resolve_scope(ScopeKind::Task, None, dir.path()).expect_err("missing task");
        assert!(error.to_string().contains("requires --scope-id"));
    }

    #[test]
    fn non_path_project_scope_ids_remain_logical_ids() {
        let dir = tempdir().expect("tempdir");
        let resolved = resolve_project_scope_id(Some("logical-project-id".to_string()), dir.path())
            .expect("scope");
        assert_eq!(resolved, "logical-project-id");
    }
}
