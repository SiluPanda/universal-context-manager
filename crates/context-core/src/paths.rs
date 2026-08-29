use crate::error::{ContextError, ContextResult};
use directories::{BaseDirs, ProjectDirs};
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const UNIX_SOCKET_PATH_LIMIT: usize = 96;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextPaths {
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub socket_path: PathBuf,
    pub spool_dir: PathBuf,
}

impl ContextPaths {
    pub fn discover() -> ContextResult<Self> {
        let project_dirs =
            ProjectDirs::from("com", "universalcontextmanager", "UniversalContextManager")
                .ok_or_else(|| ContextError::validation("unable to resolve project directories"))?;
        let manager_home = env_path("CONTEXT_MANAGER_HOME")?;
        let data_dir = env_path("CONTEXT_DATA_DIR")?
            .or(manager_home.clone())
            .unwrap_or_else(|| project_dirs.data_local_dir().to_path_buf());
        let db_path = env_path("CONTEXT_DB_PATH")?.unwrap_or_else(|| data_dir.join("context.db"));
        let socket_path = env_path("CONTEXT_SOCKET_PATH")?
            .unwrap_or_else(|| default_socket_path(&data_dir, &short_socket_root()));
        let spool_dir = env_path("CONTEXT_SPOOL_DIR")?.unwrap_or_else(|| data_dir.join("spool"));
        Ok(Self {
            data_dir,
            db_path,
            socket_path,
            spool_dir,
        })
    }

    pub fn ensure_parent_dirs(&self) -> ContextResult<()> {
        ensure_dir(&absolute_path(&self.data_dir)?)?;
        if let Some(parent) = self.db_path.parent() {
            ensure_dir(&absolute_path(parent)?)?;
        }
        if let Some(parent) = self.socket_path.parent() {
            ensure_dir(&absolute_path(parent)?)?;
        }
        ensure_dir(&absolute_path(&self.spool_dir)?)?;
        Ok(())
    }
}

pub fn normalize_project_scope_id(project_scope_id: &str) -> ContextResult<String> {
    let trimmed = project_scope_id.trim();
    if trimmed.is_empty() {
        return Err(ContextError::validation("project scope id is required"));
    }
    let candidate = PathBuf::from(trimmed);
    if !candidate.is_dir() {
        return Ok(trimmed.to_string());
    }

    let canonical = candidate.canonicalize()?;
    let normalized = discover_git_root(&canonical).unwrap_or(canonical);
    Ok(normalized.display().to_string())
}

fn discover_git_root(directory: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8(output.stdout).ok()?;
    let root = root.trim();
    if root.is_empty() {
        return None;
    }
    let root = PathBuf::from(root);
    Some(root.canonicalize().unwrap_or(root))
}

fn env_path(key: &str) -> ContextResult<Option<PathBuf>> {
    let Some(raw) = env::var_os(key) else {
        return Ok(None);
    };
    let path = PathBuf::from(raw);
    if path.as_os_str().is_empty() || path.to_string_lossy().trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(absolute_path(&path)?))
    }
}

fn absolute_path(path: &Path) -> ContextResult<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

fn default_socket_path(data_dir: &Path, short_root: &Path) -> PathBuf {
    let preferred = data_dir.join("contextd.sock");
    if unix_socket_path_fits(&preferred) {
        return preferred;
    }

    let mut hasher = Sha256::new();
    hasher.update(data_dir.to_string_lossy().as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    let fallback = short_root.join(format!("ucm-{}.sock", &hash[..16]));
    if unix_socket_path_fits(&fallback) {
        fallback
    } else {
        env::temp_dir().join(format!("ucm-{}.sock", &hash[..12]))
    }
}

fn short_socket_root() -> PathBuf {
    BaseDirs::new()
        .map(|dirs| dirs.cache_dir().join("ucm"))
        .unwrap_or_else(|| env::temp_dir().join("ucm"))
}

fn unix_socket_path_fits(path: &Path) -> bool {
    #[cfg(unix)]
    {
        path.as_os_str().to_string_lossy().len() < UNIX_SOCKET_PATH_LIMIT
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

fn ensure_dir(path: &Path) -> ContextResult<()> {
    let mut created = Vec::new();
    let mut current = path;
    while !current.exists() {
        created.push(current.to_path_buf());
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent;
    }
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for dir in created.iter().rev() {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn ignores_empty_env_paths() {
        let _guard = env_lock().lock().expect("env lock");
        let old_manager_home = env::var_os("CONTEXT_MANAGER_HOME");
        let old_data_dir = env::var_os("CONTEXT_DATA_DIR");
        let old_socket_path = env::var_os("CONTEXT_SOCKET_PATH");
        unsafe {
            env::set_var("CONTEXT_MANAGER_HOME", "   ");
            env::set_var("CONTEXT_DATA_DIR", "");
            env::set_var("CONTEXT_SOCKET_PATH", " ");
        }
        assert!(env_path("CONTEXT_MANAGER_HOME").expect("path").is_none());
        assert!(env_path("CONTEXT_DATA_DIR").expect("path").is_none());
        assert!(env_path("CONTEXT_SOCKET_PATH").expect("path").is_none());
        unsafe {
            match old_manager_home {
                Some(value) => env::set_var("CONTEXT_MANAGER_HOME", value),
                None => env::remove_var("CONTEXT_MANAGER_HOME"),
            }
            match old_data_dir {
                Some(value) => env::set_var("CONTEXT_DATA_DIR", value),
                None => env::remove_var("CONTEXT_DATA_DIR"),
            }
            match old_socket_path {
                Some(value) => env::set_var("CONTEXT_SOCKET_PATH", value),
                None => env::remove_var("CONTEXT_SOCKET_PATH"),
            }
        }
    }

    #[test]
    fn relative_env_paths_are_absolutized() {
        let _guard = env_lock().lock().expect("env lock");
        let old_manager_home = env::var_os("CONTEXT_MANAGER_HOME");
        let cwd = env::current_dir().expect("cwd");
        unsafe {
            env::set_var("CONTEXT_MANAGER_HOME", "relative-ucm-home");
        }
        let path = env_path("CONTEXT_MANAGER_HOME")
            .expect("path")
            .expect("some path");
        assert_eq!(path, cwd.join("relative-ucm-home"));
        unsafe {
            match old_manager_home {
                Some(value) => env::set_var("CONTEXT_MANAGER_HOME", value),
                None => env::remove_var("CONTEXT_MANAGER_HOME"),
            }
        }
    }

    #[test]
    fn preserves_permissions_for_existing_custom_dirs() {
        let dir = tempdir().expect("tempdir");
        let existing = dir.path().join("project");
        fs::create_dir_all(&existing).expect("mkdir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&existing, fs::Permissions::from_mode(0o755)).expect("chmod");
        }

        let paths = ContextPaths {
            data_dir: existing.clone(),
            db_path: existing.join("context.db"),
            socket_path: existing.join("contextd.sock"),
            spool_dir: existing.join("spool"),
        };
        paths.ensure_parent_dirs().expect("ensure dirs");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let existing_mode = fs::metadata(&existing)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            let spool_mode = fs::metadata(paths.spool_dir)
                .expect("spool metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(existing_mode, 0o755);
            assert_eq!(spool_mode, 0o700);
        }
    }

    #[test]
    fn newly_created_dirs_are_private() {
        let dir = tempdir().expect("tempdir");
        let ucm_root = dir.path().join("ucm-root");
        let data_dir = ucm_root.join("nested/data");
        let paths = ContextPaths {
            data_dir: data_dir.clone(),
            db_path: data_dir.join("context.db"),
            socket_path: data_dir.join("contextd.sock"),
            spool_dir: data_dir.join("spool"),
        };
        paths.ensure_parent_dirs().expect("ensure dirs");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let data_mode = fs::metadata(&data_dir)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            let root_mode = fs::metadata(&ucm_root)
                .expect("root metadata")
                .permissions()
                .mode()
                & 0o777;
            let spool_mode = fs::metadata(paths.spool_dir)
                .expect("spool metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(root_mode, 0o700);
            assert_eq!(data_mode, 0o700);
            assert_eq!(spool_mode, 0o700);
        }
    }

    #[test]
    fn logical_project_scope_ids_are_preserved() {
        assert_eq!(
            normalize_project_scope_id("logical-project-id").expect("scope"),
            "logical-project-id"
        );
        assert_eq!(
            normalize_project_scope_id("  logical-project-id  ").expect("trimmed scope"),
            "logical-project-id"
        );
        assert!(normalize_project_scope_id("   ").is_err());
    }

    #[test]
    fn existing_project_directory_falls_back_to_canonical_path() {
        let dir = tempdir().expect("tempdir");
        let project = dir.path().join("project");
        fs::create_dir_all(&project).expect("project");

        assert_eq!(
            PathBuf::from(
                normalize_project_scope_id(project.to_str().expect("utf-8 path")).expect("scope")
            ),
            project.canonicalize().expect("canonical project")
        );
    }

    #[cfg(unix)]
    #[test]
    fn project_symlink_and_nested_directory_resolve_to_canonical_git_root() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().expect("tempdir");
        let root = dir.path().join("repository");
        let nested = root.join("nested/project");
        fs::create_dir_all(&nested).expect("nested");
        let status = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "--quiet"])
            .status()
            .expect("git init");
        assert!(status.success());
        let alias = dir.path().join("repository-alias");
        symlink(&nested, &alias).expect("symlink");

        let canonical_root = root.canonicalize().expect("canonical root");
        assert_eq!(
            PathBuf::from(
                normalize_project_scope_id(alias.to_str().expect("utf-8 alias")).expect("alias")
            ),
            canonical_root
        );
        assert_eq!(
            PathBuf::from(
                normalize_project_scope_id(nested.to_str().expect("utf-8 nested")).expect("nested")
            ),
            canonical_root
        );
    }

    #[test]
    fn default_socket_path_is_short_and_deterministic() {
        let dir = tempdir().expect("tempdir");
        let data_dir = dir
            .path()
            .join("very")
            .join("long")
            .join("nested")
            .join("data")
            .join("directory")
            .join("that")
            .join("should")
            .join("not")
            .join("appear")
            .join("in")
            .join("socket");
        let short_root = dir.path().join("runtime-root");
        let socket_a = default_socket_path(&data_dir, &short_root);
        let socket_b = default_socket_path(&data_dir, &short_root);
        assert_eq!(socket_a, socket_b);
        #[cfg(unix)]
        {
            use std::os::unix::net::UnixListener;

            let len = socket_a.as_os_str().to_string_lossy().len();
            assert!(len < UNIX_SOCKET_PATH_LIMIT, "socket path too long: {len}");
            if let Some(parent) = socket_a.parent() {
                ensure_dir(parent).expect("runtime dir");
            }
            let listener = UnixListener::bind(&socket_a).expect("bind socket");
            drop(listener);
            fs::remove_file(&socket_a).expect("remove socket");
        }
    }

    #[test]
    fn discover_preserves_manager_home_socket_when_short() {
        let _guard = env_lock().lock().expect("env lock");
        let dir = tempdir().expect("tempdir");
        let home = dir.path().join("ucm-home");
        let old_manager_home = env::var_os("CONTEXT_MANAGER_HOME");
        let old_data_dir = env::var_os("CONTEXT_DATA_DIR");
        let old_db_path = env::var_os("CONTEXT_DB_PATH");
        let old_socket_path = env::var_os("CONTEXT_SOCKET_PATH");
        let old_spool_dir = env::var_os("CONTEXT_SPOOL_DIR");
        unsafe {
            env::set_var("CONTEXT_MANAGER_HOME", &home);
            env::remove_var("CONTEXT_DATA_DIR");
            env::remove_var("CONTEXT_DB_PATH");
            env::remove_var("CONTEXT_SOCKET_PATH");
            env::remove_var("CONTEXT_SPOOL_DIR");
        }

        let paths = ContextPaths::discover().expect("discover");
        assert_eq!(paths.data_dir, home);
        assert_eq!(paths.socket_path, home.join("contextd.sock"));

        unsafe {
            match old_manager_home {
                Some(value) => env::set_var("CONTEXT_MANAGER_HOME", value),
                None => env::remove_var("CONTEXT_MANAGER_HOME"),
            }
            match old_data_dir {
                Some(value) => env::set_var("CONTEXT_DATA_DIR", value),
                None => env::remove_var("CONTEXT_DATA_DIR"),
            }
            match old_db_path {
                Some(value) => env::set_var("CONTEXT_DB_PATH", value),
                None => env::remove_var("CONTEXT_DB_PATH"),
            }
            match old_socket_path {
                Some(value) => env::set_var("CONTEXT_SOCKET_PATH", value),
                None => env::remove_var("CONTEXT_SOCKET_PATH"),
            }
            match old_spool_dir {
                Some(value) => env::set_var("CONTEXT_SPOOL_DIR", value),
                None => env::remove_var("CONTEXT_SPOOL_DIR"),
            }
        }
    }

    #[test]
    fn discover_uses_short_socket_path_when_default_data_dir_is_long() {
        let _guard = env_lock().lock().expect("env lock");
        let dir = tempdir().expect("tempdir");
        let fake_home = dir
            .path()
            .join("home")
            .join("with-a-pretty-long-user-name-for-macos-tests");
        fs::create_dir_all(&fake_home).expect("mkdir");
        let old_home = env::var_os("HOME");
        let old_manager_home = env::var_os("CONTEXT_MANAGER_HOME");
        let old_data_dir = env::var_os("CONTEXT_DATA_DIR");
        let old_db_path = env::var_os("CONTEXT_DB_PATH");
        let old_socket_path = env::var_os("CONTEXT_SOCKET_PATH");
        let old_spool_dir = env::var_os("CONTEXT_SPOOL_DIR");

        unsafe {
            env::set_var("HOME", &fake_home);
            env::remove_var("CONTEXT_MANAGER_HOME");
            env::remove_var("CONTEXT_DATA_DIR");
            env::remove_var("CONTEXT_DB_PATH");
            env::remove_var("CONTEXT_SOCKET_PATH");
            env::remove_var("CONTEXT_SPOOL_DIR");
        }

        let paths = ContextPaths::discover().expect("discover");
        #[cfg(unix)]
        {
            use std::os::unix::net::UnixListener;

            let preferred = paths.data_dir.join("contextd.sock");
            let socket_len = paths.socket_path.as_os_str().to_string_lossy().len();
            assert!(
                socket_len < UNIX_SOCKET_PATH_LIMIT,
                "socket path too long: {socket_len}"
            );
            if unix_socket_path_fits(&preferred) {
                assert_eq!(paths.socket_path, preferred);
            } else {
                assert_ne!(paths.socket_path, preferred);
            }
            if let Some(parent) = paths.socket_path.parent() {
                ensure_dir(parent).expect("socket parent");
            }
            let listener = UnixListener::bind(&paths.socket_path).expect("bind socket");
            drop(listener);
            fs::remove_file(&paths.socket_path).expect("remove socket");
        }

        unsafe {
            match old_home {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }
            match old_manager_home {
                Some(value) => env::set_var("CONTEXT_MANAGER_HOME", value),
                None => env::remove_var("CONTEXT_MANAGER_HOME"),
            }
            match old_data_dir {
                Some(value) => env::set_var("CONTEXT_DATA_DIR", value),
                None => env::remove_var("CONTEXT_DATA_DIR"),
            }
            match old_db_path {
                Some(value) => env::set_var("CONTEXT_DB_PATH", value),
                None => env::remove_var("CONTEXT_DB_PATH"),
            }
            match old_socket_path {
                Some(value) => env::set_var("CONTEXT_SOCKET_PATH", value),
                None => env::remove_var("CONTEXT_SOCKET_PATH"),
            }
            match old_spool_dir {
                Some(value) => env::set_var("CONTEXT_SPOOL_DIR", value),
                None => env::remove_var("CONTEXT_SPOOL_DIR"),
            }
        }
    }
}
