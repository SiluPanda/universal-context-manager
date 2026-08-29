use anyhow::{Context, Result, anyhow};
use context_core::SourceImportDocument;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn canonicalize_source_path(path: &Path, base: &Path) -> Result<PathBuf> {
    let selected = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let canonical = selected
        .canonicalize()
        .with_context(|| format!("resolve source {}", selected.display()))?;
    if !canonical.is_file() {
        return Err(anyhow!(
            "source is not a regular file: {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

pub(crate) fn canonicalize_source_paths(
    paths: impl IntoIterator<Item = PathBuf>,
    base: &Path,
) -> Result<Vec<PathBuf>> {
    paths
        .into_iter()
        .map(|path| canonicalize_source_path(&path, base))
        .collect()
}

pub(crate) fn read_source_documents(
    paths: &[PathBuf],
    project_root: Option<&Path>,
) -> Result<Vec<SourceImportDocument>> {
    let canonical_project = project_root.and_then(|project| project.canonicalize().ok());
    paths
        .iter()
        .map(|path| {
            let canonical = path
                .canonicalize()
                .with_context(|| format!("resolve source {}", path.display()))?;
            if !canonical.is_file() {
                return Err(anyhow!(
                    "source is not a regular file: {}",
                    canonical.display()
                ));
            }
            let payload = fs::read_to_string(&canonical)
                .with_context(|| format!("read {}", canonical.display()))?;
            Ok(SourceImportDocument {
                path: Some(source_identity(&canonical, canonical_project.as_deref())),
                payload,
            })
        })
        .collect()
}

pub(crate) fn source_identity(path: &Path, project_root: Option<&Path>) -> String {
    let selected = project_root
        .and_then(|project| path.strip_prefix(project).ok())
        .filter(|relative| !relative.as_os_str().is_empty())
        .unwrap_or(path);
    selected.display().to_string().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir_in;

    #[test]
    fn source_identity_is_project_relative_or_canonical_absolute() {
        let workspace = tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("workspace");
        let project = workspace.path().join("project");
        let outside = workspace.path().join("outside.md");
        let inside = project.join("rules/AGENTS.md");
        fs::create_dir_all(inside.parent().expect("parent")).expect("mkdir");
        fs::write(&inside, "# Inside").expect("inside");
        fs::write(&outside, "# Outside").expect("outside");

        let inside = inside.canonicalize().expect("inside canonical");
        let outside = outside.canonicalize().expect("outside canonical");
        let project = project.canonicalize().expect("project canonical");
        assert_eq!(source_identity(&inside, Some(&project)), "rules/AGENTS.md");
        assert_eq!(
            source_identity(&outside, Some(&project)),
            outside.display().to_string()
        );
    }
}
