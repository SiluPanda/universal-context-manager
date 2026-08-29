use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use chrono::Utc;

use crate::models::{PathGrantPurpose, PathGrantSelection};

const GRANT_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Default)]
pub(crate) struct PathGrantStore {
    grants: Arc<Mutex<HashMap<String, PathGrant>>>,
}

struct PathGrant {
    purpose: PathGrantPurpose,
    paths: Vec<PathBuf>,
    expires_at: Instant,
}

impl PathGrantStore {
    pub(crate) fn issue(
        &self,
        purpose: PathGrantPurpose,
        paths: Vec<PathBuf>,
    ) -> Result<PathGrantSelection, String> {
        let canonical = canonicalize_paths(purpose, &paths)?;
        self.issue_canonical(purpose, canonical)
    }

    pub(crate) fn issue_canonical(
        &self,
        purpose: PathGrantPurpose,
        paths: Vec<PathBuf>,
    ) -> Result<PathGrantSelection, String> {
        validate_path_count(purpose, paths.len())?;
        let token = format!("path-grant-{}", context_core::new_id());
        let expires_at = Instant::now() + GRANT_TTL;
        let expires_at_text = (Utc::now()
            + chrono::Duration::from_std(GRANT_TTL).map_err(|error| error.to_string())?)
        .to_rfc3339();
        let mut grants = self
            .grants
            .lock()
            .map_err(|_| "path grant store is unavailable".to_string())?;
        grants.retain(|_, grant| grant.expires_at > Instant::now());
        grants.insert(
            token.clone(),
            PathGrant {
                purpose,
                paths: paths.clone(),
                expires_at,
            },
        );
        Ok(PathGrantSelection {
            grant_token: token,
            purpose,
            paths: paths
                .into_iter()
                .map(|path| path.display().to_string())
                .collect(),
            expires_at: expires_at_text,
        })
    }

    pub(crate) fn consume(
        &self,
        purpose: PathGrantPurpose,
        token: Option<&str>,
        requested_paths: &[String],
    ) -> Result<Vec<PathBuf>, String> {
        let token = token
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| "path grant is required for this operation".to_string())?;
        let grant = self
            .grants
            .lock()
            .map_err(|_| "path grant store is unavailable".to_string())?
            .remove(token)
            .ok_or_else(|| "path grant is invalid, expired, or already used".to_string())?;
        if grant.expires_at <= Instant::now() {
            return Err("path grant has expired".to_string());
        }
        if grant.purpose != purpose {
            return Err("path grant does not authorize this operation".to_string());
        }
        let requested = requested_paths
            .iter()
            .map(|path| PathBuf::from(path.trim()))
            .collect::<Vec<_>>();
        let canonical = canonicalize_paths(purpose, &requested)?;
        if canonical != grant.paths {
            return Err("path grant does not match the requested path selection".to_string());
        }
        Ok(grant.paths)
    }
}

fn canonicalize_paths(
    purpose: PathGrantPurpose,
    paths: &[PathBuf],
) -> Result<Vec<PathBuf>, String> {
    validate_path_count(purpose, paths.len())?;
    paths
        .iter()
        .map(|path| match purpose {
            PathGrantPurpose::ProjectRegistration => canonical_directory(path),
            PathGrantPurpose::SourceImportPreview
            | PathGrantPurpose::SourceImportApply
            | PathGrantPurpose::BundleImportPreview
            | PathGrantPurpose::BundleImportApply => canonical_input_file(path),
            PathGrantPurpose::ExportArchive => canonical_output_file(path),
        })
        .collect()
}

fn validate_path_count(purpose: PathGrantPurpose, count: usize) -> Result<(), String> {
    match purpose {
        PathGrantPurpose::SourceImportPreview | PathGrantPurpose::SourceImportApply
            if (1..=32).contains(&count) =>
        {
            Ok(())
        }
        PathGrantPurpose::SourceImportPreview | PathGrantPurpose::SourceImportApply => {
            Err("path grant requires between one and 32 source files".to_string())
        }
        _ if count == 1 => Ok(()),
        _ => Err("path grant requires exactly one selected path".to_string()),
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    let canonical = fs::canonicalize(path).map_err(|error| error.to_string())?;
    if !canonical.is_dir() {
        return Err("selected project path is not a directory".to_string());
    }
    Ok(canonical)
}

fn canonical_input_file(path: &Path) -> Result<PathBuf, String> {
    let source_metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if source_metadata.file_type().is_symlink() {
        return Err("selected input paths must not be symbolic links".to_string());
    }
    let canonical = fs::canonicalize(path).map_err(|error| error.to_string())?;
    if !canonical.is_file() {
        return Err("selected input path is not a regular file".to_string());
    }
    Ok(canonical)
}

fn canonical_output_file(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("selected export path must not be empty".to_string());
    }
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err("selected export path must not be a symbolic link".to_string());
        }
        if !metadata.is_file() {
            return Err("selected export path is not a regular file".to_string());
        }
        return fs::canonicalize(path).map_err(|error| error.to_string());
    }
    let file_name = path
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "selected export path requires a file name".to_string())?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "selected export path requires a parent directory".to_string())?;
    let canonical_parent = fs::canonicalize(parent).map_err(|error| error.to_string())?;
    if !canonical_parent.is_dir() {
        return Err("selected export parent is not a directory".to_string());
    }
    Ok(canonical_parent.join(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn path_grants_reject_mismatch_and_replay() {
        let temp = tempdir().expect("tempdir");
        let first = temp.path().join("first.md");
        let second = temp.path().join("second.md");
        fs::write(&first, "first").expect("first file");
        fs::write(&second, "second").expect("second file");
        let store = PathGrantStore::default();
        let grant = store
            .issue(PathGrantPurpose::SourceImportPreview, vec![first.clone()])
            .expect("issue grant");
        let mismatch = store
            .consume(
                PathGrantPurpose::SourceImportPreview,
                Some(&grant.grant_token),
                &[second.display().to_string()],
            )
            .expect_err("mismatch");
        assert!(mismatch.contains("does not match"));
        let replay = store
            .consume(
                PathGrantPurpose::SourceImportPreview,
                Some(&grant.grant_token),
                &[first.display().to_string()],
            )
            .expect_err("consumed mismatch grant cannot replay");
        assert!(replay.contains("already used"));
    }

    #[test]
    fn path_grants_are_one_time_and_operation_bound() {
        let temp = tempdir().expect("tempdir");
        let store = PathGrantStore::default();
        let grant = store
            .issue(
                PathGrantPurpose::ProjectRegistration,
                vec![temp.path().to_path_buf()],
            )
            .expect("project grant");
        let mismatch = store
            .consume(
                PathGrantPurpose::BundleImportPreview,
                Some(&grant.grant_token),
                &[temp.path().display().to_string()],
            )
            .expect_err("purpose mismatch");
        assert!(mismatch.contains("does not authorize"));
        assert!(store
            .consume(
                PathGrantPurpose::ProjectRegistration,
                Some(&grant.grant_token),
                &[temp.path().display().to_string()],
            )
            .is_err());
    }
}
