use super::parser::process_omp_conversation_file;
use super::{Conversation, Source, format_short_name_from_path};
use crate::debug;
use crate::debug::DebugLevel;
use crate::env::HistoryEnv;
use crate::error::{AppError, Result};
use std::fs::read_dir;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OmpSessionRoot {
    pub path: PathBuf,
    pub flat: bool,
}

/// The OMP session root for this environment. Relative `PI_*` paths and a
/// relative `XDG_DATA_HOME` resolve against the launch directory, else the
/// workspace directory (upstream: the process cwd;
/// [`HistoryEnv::env_value_base_dir`], K16). Without either, a relative
/// `XDG_DATA_HOME` is ignored (the XDG spec calls it invalid) and a
/// relative `PI_*` path is an error. A base that is not an absolute path is
/// ignored.
pub fn session_root(env: &HistoryEnv) -> Result<OmpSessionRoot> {
    let base = env.env_value_base_dir();
    let xdg_data_home = env.xdg_data_home.clone().and_then(|root| {
        if root.is_absolute() {
            Some(root)
        } else {
            base.as_ref().map(|base| base.join(root))
        }
    });
    session_root_from(
        env.pi_config_dir.clone(),
        env.pi_coding_agent_dir.clone(),
        env.pi_coding_agent_session_dir.clone(),
        env.omp_profile.clone(),
        env.pi_profile.clone(),
        xdg_data_home,
        env.home_dir.clone(),
        base.as_deref(),
    )
}

#[allow(clippy::too_many_arguments)]
fn session_root_from(
    config_dir: Option<PathBuf>,
    agent_override: Option<PathBuf>,
    session_override: Option<PathBuf>,
    omp_profile: Option<std::ffi::OsString>,
    pi_profile: Option<std::ffi::OsString>,
    xdg_data_home: Option<PathBuf>,
    home_dir: Option<PathBuf>,
    base: Option<&Path>,
) -> Result<OmpSessionRoot> {
    let home = home_dir.ok_or_else(|| {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine home directory",
        ))
    })?;
    if let Some(path) = session_override {
        return Ok(OmpSessionRoot {
            path: expand_path_with_home(path, &home, base)?,
            flat: true,
        });
    }

    let profile_value = omp_profile.or(pi_profile);
    let profile = profile_value
        .as_deref()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "default");
    let config_root = home.join(config_dir.unwrap_or_else(|| PathBuf::from(".omp")));
    let profile_root = profile
        .map(|profile| config_root.join("profiles").join(profile))
        .unwrap_or(config_root);
    let default_agent = profile_root.join("agent");
    let agent_dir = if profile.is_none() {
        match agent_override {
            Some(path) => expand_path_with_home(path, &home, base)?,
            None => default_agent,
        }
    } else {
        default_agent
    };

    // The XDG spec says a relative XDG_DATA_HOME is invalid and must be
    // ignored; upstream probed it against the process cwd (D2).
    let xdg_profile_root = xdg_data_home.filter(|root| root.is_absolute()).map(|root| {
        let root = root.join("omp");
        profile
            .map(|profile| root.join("profiles").join(profile))
            .unwrap_or(root)
    });
    let path = if agent_dir == profile_root.join("agent")
        && let Some(xdg_root) = xdg_profile_root
        && xdg_root.exists()
    {
        xdg_root.join("sessions")
    } else {
        agent_dir.join("sessions")
    };
    Ok(OmpSessionRoot { path, flat: false })
}

/// Expand `~`/`~/…` and resolve relative paths against `base` (the
/// workspace; upstream used the process cwd and fell back to `.`). Without a
/// base a relative path is an error, as for Pi: a GUI's process working
/// directory is arbitrary (D2).
fn expand_path_with_home(path: PathBuf, home: &Path, base: Option<&Path>) -> Result<PathBuf> {
    let expanded = if path == Path::new("~") {
        home.to_path_buf()
    } else if let Ok(rest) = path.strip_prefix("~/") {
        home.join(rest)
    } else {
        path
    };
    if expanded.is_absolute() {
        return Ok(expanded);
    }
    let base = base.ok_or_else(|| {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Cannot resolve relative OMP path '{}' without a workspace directory",
                expanded.display()
            ),
        ))
    })?;
    Ok(base.join(expanded))
}

pub fn discover_files(root: &OmpSessionRoot) -> Result<Vec<PathBuf>> {
    if !root.path.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    if root.flat {
        collect_jsonl(&root.path, &mut files)?;
    } else {
        for entry in read_dir(&root.path)? {
            let path = entry?.path();
            if path.is_dir() {
                collect_jsonl(&path, &mut files)?;
            }
        }
    }
    files.sort();
    Ok(files)
}

fn collect_jsonl(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in read_dir(directory)? {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    Ok(())
}

pub fn load_omp_conversations(
    env: &HistoryEnv,
    show_last: bool,
    debug_level: Option<DebugLevel>,
) -> Result<Vec<Conversation>> {
    let root = session_root(env)?;
    let files = discover_files(&root)?;
    let cached = super::cache::read_omp_cache(env, &root.path).unwrap_or_default();
    let mut updated_cache = std::collections::HashMap::new();
    let mut conversations = Vec::new();
    for path in files {
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        let modified = metadata.modified().ok();
        let cache_key = path
            .strip_prefix(&root.path)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        let mut conversation = modified
            .and_then(|mtime| {
                cached.get(&cache_key).filter(|entry| {
                    super::cache::entry_matches(&entry.metadata, metadata.len(), mtime)
                })
            })
            .map(|entry| {
                let mut conversation =
                    super::cache::conversation_from_entry(&entry.metadata, path.clone(), show_last);
                conversation.source = Source::Omp;
                conversation.session_id = entry.session_id.clone();
                conversation.cwd = Some(entry.project_path.clone());
                conversation.project_path = Some(entry.project_path.clone());
                conversation.project_name = Some(format_short_name_from_path(&entry.project_path));
                conversation
            });
        if conversation.is_none() {
            let root_is_omp_owned = root
                .path
                .components()
                .any(|component| matches!(component.as_os_str().to_str(), Some(".omp" | "omp")));
            let parsed = if root_is_omp_owned {
                process_omp_conversation_file(path.clone(), modified, debug_level)
            } else {
                super::parser::process_conversation_file(path.clone(), modified, debug_level)
            };
            conversation = match parsed {
                Ok(Some(conversation)) if conversation.source == Source::Omp => Some(conversation),
                Ok(_) => None,
                Err(error) => {
                    debug::warn(
                        debug_level,
                        &format!("Failed to parse OMP session {}: {error}", path.display()),
                    );
                    None
                }
            };
        }
        let Some(mut conversation) = conversation else {
            continue;
        };
        conversation.preview = if show_last {
            conversation.preview_last.clone()
        } else {
            conversation.preview_first.clone()
        };
        let project_path = conversation
            .cwd
            .clone()
            .unwrap_or_else(|| PathBuf::from("unknown"));
        conversation.project_name = Some(format_short_name_from_path(&project_path));
        conversation.project_path = Some(project_path.clone());
        if let Some(mtime) = modified {
            updated_cache.insert(
                cache_key,
                super::cache::PiCacheEntry {
                    metadata: super::cache::entry_from_conversation(
                        &conversation,
                        metadata.len(),
                        mtime,
                    ),
                    session_id: conversation.session_id.clone(),
                    project_path,
                },
            );
        }
        conversations.push(conversation);
    }
    super::cache::write_omp_cache(env, &root.path, updated_cache);
    conversations.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
    for (index, conversation) in conversations.iter_mut().enumerate() {
        conversation.index = index;
    }
    Ok(conversations)
}

pub fn delete_session(path: &Path) -> Result<()> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl")
        || !super::pi::is_omp_file(path)
    {
        return Err(AppError::SessionNotFound(path.display().to_string()));
    }
    std::fs::remove_file(path)?;
    if let Some(artifacts) = session_artifact_dir(path)
        && artifacts.is_dir()
    {
        std::fs::remove_dir_all(artifacts)?;
    }
    Ok(())
}

/// The artifact directory next to an OMP session file: `<dir>/<stem>/` for
/// `<dir>/<stem>.jsonl` (upstream `path.with_extension("")`).
///
/// `None` unless the stem is a plain file name: `...jsonl` (stem `..`) and
/// `..jsonl` (stem `.`) would name the sessions root or the session's own
/// directory, and a stem ending in `.` or ` ` (e.g. `x..jsonl`) would name
/// another session's directory once Windows strips the trailing character.
pub fn session_artifact_dir(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?;
    let text = stem.to_str()?;
    let mut components = Path::new(stem).components();
    let plain = matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(name)), None) if name == stem
    );
    if !plain || text.ends_with(['.', ' ']) || text.contains(':') {
        return None;
    }
    Some(path.parent()?.join(stem))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_default_profile_overrides_and_xdg_roots() {
        let home = tempfile::tempdir().unwrap();
        let default = session_root_from(
            None,
            None,
            None,
            None,
            None,
            None,
            Some(home.path().to_path_buf()),
            None,
        )
        .unwrap();
        assert_eq!(default.path, home.path().join(".omp/agent/sessions"));
        assert!(!default.flat);

        let profile = session_root_from(
            None,
            Some(PathBuf::from("~/ignored")),
            None,
            Some("work".into()),
            None,
            None,
            Some(home.path().to_path_buf()),
            None,
        )
        .unwrap();
        assert_eq!(
            profile.path,
            home.path().join(".omp/profiles/work/agent/sessions")
        );

        let xdg = home.path().join("xdg/omp");
        std::fs::create_dir_all(&xdg).unwrap();
        let xdg_root = session_root_from(
            None,
            None,
            None,
            None,
            None,
            Some(home.path().join("xdg")),
            Some(home.path().to_path_buf()),
            None,
        )
        .unwrap();
        assert_eq!(xdg_root.path, xdg.join("sessions"));

        let custom = session_root_from(
            None,
            None,
            Some(PathBuf::from("~/sessions")),
            None,
            None,
            None,
            Some(home.path().to_path_buf()),
            None,
        )
        .unwrap();
        assert_eq!(custom.path, home.path().join("sessions"));
        assert!(custom.flat);
    }

    /// `target` spelled relative to the process cwd (read, never changed),
    /// or `None` when no relative spelling exists (another drive).
    fn relative_to_process_cwd(target: &Path) -> Option<PathBuf> {
        use std::path::Component;
        let cwd = std::env::current_dir().ok()?;
        let anchor = |path: &Path| {
            path.components()
                .take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
                .collect::<PathBuf>()
        };
        if anchor(&cwd) != anchor(target) {
            return None;
        }
        let normal = |path: &Path| {
            path.components()
                .filter(|c| matches!(c, Component::Normal(_)))
                .count()
        };
        let mut relative = PathBuf::new();
        for _ in 0..normal(&cwd) {
            relative.push("..");
        }
        for component in target.components() {
            if let Component::Normal(part) = component {
                relative.push(part);
            }
        }
        Some(relative)
    }

    #[test]
    fn relative_xdg_data_home_is_ignored() {
        // The XDG spec says a relative XDG_DATA_HOME is invalid; upstream
        // probed `<process cwd>/<value>/omp` and listed its sessions.
        let home = tempfile::tempdir().unwrap();
        let xdg = home.path().join("xdg");
        std::fs::create_dir_all(xdg.join("omp")).unwrap();
        let default = home.path().join(".omp/agent/sessions");
        let mut relatives = vec![PathBuf::from("xdg"), PathBuf::from("rel")];
        // A relative spelling that really reaches the existing directory
        // from the test's cwd, so the probe would succeed without the fix.
        relatives.extend(relative_to_process_cwd(&xdg));
        for relative in relatives {
            let root = session_root_from(
                None,
                None,
                None,
                None,
                None,
                Some(relative.clone()),
                Some(home.path().to_path_buf()),
                None,
            )
            .unwrap();
            assert_eq!(root.path, default, "{}", relative.display());
        }
        // The absolute spelling is still used.
        let env = HistoryEnv {
            xdg_data_home: Some(xdg.clone()),
            ..HistoryEnv::sandbox(home.path())
        };
        assert_eq!(session_root(&env).unwrap().path, xdg.join("omp/sessions"));
    }

    #[test]
    fn c045_pi_config_dir_and_profiles_select_the_omp_root() {
        let home = tempfile::tempdir().unwrap();
        let base = HistoryEnv::sandbox(home.path());
        // PI_CONFIG_DIR is relative to the home directory.
        let env = HistoryEnv {
            pi_config_dir: Some(PathBuf::from(".omp-custom")),
            ..base.clone()
        };
        assert_eq!(
            session_root(&env).unwrap().path,
            home.path().join(".omp-custom/agent/sessions")
        );
        // OMP_PROFILE wins over PI_PROFILE; PI_PROFILE alone also selects.
        let env = HistoryEnv {
            omp_profile: Some("omp-profile".into()),
            pi_profile: Some("pi-profile".into()),
            ..base.clone()
        };
        assert_eq!(
            session_root(&env).unwrap().path,
            home.path().join(".omp/profiles/omp-profile/agent/sessions")
        );
        let env = HistoryEnv {
            pi_profile: Some("pi-profile".into()),
            ..base.clone()
        };
        assert_eq!(
            session_root(&env).unwrap().path,
            home.path().join(".omp/profiles/pi-profile/agent/sessions")
        );
        // `default` is no profile.
        let env = HistoryEnv {
            omp_profile: Some("default".into()),
            ..base.clone()
        };
        assert_eq!(
            session_root(&env).unwrap().path,
            home.path().join(".omp/agent/sessions")
        );
        // An existing XDG_DATA_HOME/omp root is used for the default agent.
        let xdg = home.path().join("xdg");
        std::fs::create_dir_all(xdg.join("omp")).unwrap();
        let env = HistoryEnv {
            xdg_data_home: Some(xdg.clone()),
            ..base
        };
        assert_eq!(
            session_root(&env).unwrap().path,
            xdg.join("omp").join("sessions")
        );
    }

    #[test]
    fn relative_environment_values_resolve_against_the_launch_directory() {
        // K16: with launch directory A and workspace B, relative PI_* values
        // and a relative XDG_DATA_HOME resolve under A.
        let home = tempfile::tempdir().unwrap();
        let launch = home.path().join("launch");
        let workspace = home.path().join("workspace");
        let env = HistoryEnv::sandbox(home.path())
            .with_pi_session_dir("omp-sessions")
            .with_workspace(&workspace)
            .with_env_path_base(&launch);
        assert_eq!(
            session_root(&env).unwrap().path,
            launch.join("omp-sessions")
        );

        let xdg = launch.join("data").join("omp");
        std::fs::create_dir_all(&xdg).unwrap();
        let xdg_env = HistoryEnv {
            xdg_data_home: Some(PathBuf::from("data")),
            ..HistoryEnv::sandbox(home.path())
                .with_workspace(&workspace)
                .with_env_path_base(&launch)
        };
        assert_eq!(session_root(&xdg_env).unwrap().path, xdg.join("sessions"));
        // Without a launch directory, relative values fall back to the
        // workspace (DESIGN §4.3 K16), XDG_DATA_HOME included.
        let mut no_launch = xdg_env.clone();
        no_launch.env_path_base = None;
        let workspace_xdg = workspace.join("data").join("omp");
        std::fs::create_dir_all(&workspace_xdg).unwrap();
        assert_eq!(
            session_root(&no_launch).unwrap().path,
            workspace_xdg.join("sessions")
        );
        // A relative launch directory is no base either.
        let mut relative_launch = no_launch.clone();
        relative_launch.env_path_base = Some(PathBuf::from("launch"));
        assert_eq!(
            session_root(&relative_launch).unwrap().path,
            workspace_xdg.join("sessions")
        );
        // With neither, the relative value is ignored.
        let mut no_base = no_launch.clone();
        no_base.workspace_dir = None;
        assert_eq!(
            session_root(&no_base).unwrap().path,
            home.path().join(".omp/agent/sessions")
        );
    }

    #[test]
    fn relative_session_dir_resolves_against_the_workspace() {
        let home = tempfile::tempdir().unwrap();
        let workspace = home.path().join("workspace");
        let env = HistoryEnv::sandbox(home.path())
            .with_pi_session_dir("omp-sessions")
            .with_workspace(&workspace);
        let root = session_root(&env).unwrap();
        assert_eq!(root.path, workspace.join("omp-sessions"));
        assert!(root.flat);

        let default = session_root(&HistoryEnv::sandbox(home.path())).unwrap();
        assert_eq!(default.path, home.path().join(".omp/agent/sessions"));
    }

    #[test]
    fn relative_paths_without_a_workspace_are_errors() {
        // D2: never resolve against the GUI's arbitrary process cwd.
        let home = tempfile::tempdir().unwrap();
        for env in [
            HistoryEnv::sandbox(home.path()).with_pi_session_dir("omp-sessions"),
            HistoryEnv::sandbox(home.path()).with_pi_coding_agent_dir("agent"),
        ] {
            let error = session_root(&env).unwrap_err();
            assert!(
                error.to_string().contains("without a workspace directory"),
                "{error}"
            );
            assert!(load_omp_conversations(&env, true, None).is_err());
        }
        // `~` paths and absolute paths need no workspace.
        let tilde = HistoryEnv::sandbox(home.path()).with_pi_session_dir("~/omp");
        assert_eq!(session_root(&tilde).unwrap().path, home.path().join("omp"));
        let absolute =
            HistoryEnv::sandbox(home.path()).with_pi_coding_agent_dir(home.path().join("a"));
        assert_eq!(
            session_root(&absolute).unwrap().path,
            home.path().join("a").join("sessions")
        );
    }

    #[test]
    fn artifact_dirs_are_only_plain_stems() {
        let dir = Path::new("sessions").join("proj");
        assert_eq!(
            session_artifact_dir(&dir.join("2025_abc.jsonl")),
            Some(dir.join("2025_abc"))
        );
        for name in ["...jsonl", "..jsonl", "x..jsonl", "x .jsonl"] {
            assert_eq!(session_artifact_dir(&dir.join(name)), None, "{name}");
        }
    }

    #[test]
    fn delete_session_never_removes_the_sessions_root() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("sessions");
        let project = root.join("proj");
        let sibling = project.join("2025_other.jsonl");
        let sibling_artifacts = project.join("2025_other");
        std::fs::create_dir_all(&sibling_artifacts).unwrap();
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/omp/v3.jsonl");
        std::fs::copy(&fixture, &sibling).unwrap();
        for name in ["...jsonl", "..jsonl"] {
            let session = project.join(name);
            std::fs::copy(&fixture, &session).unwrap();
            delete_session(&session).unwrap();
            assert!(!session.exists(), "{name}");
            assert!(root.is_dir() && project.is_dir(), "{name}");
            assert!(sibling.exists() && sibling_artifacts.is_dir(), "{name}");
        }

        // A plain session still loses its artifact directory.
        let session = project.join("2025_mine.jsonl");
        let artifacts = project.join("2025_mine").join("blobs");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::copy(&fixture, &session).unwrap();
        delete_session(&session).unwrap();
        assert!(!session.exists() && !project.join("2025_mine").exists());
        assert!(sibling.exists() && sibling_artifacts.is_dir());
    }
}
