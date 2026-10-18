use super::parser::process_conversation_file;
use super::{Conversation, Source, format_short_name_from_path};
use crate::debug;
use crate::debug::DebugLevel;
use crate::env::HistoryEnv;
use crate::error::{AppError, Result};
use serde_json::Value;
use std::fs::read_dir;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PiSessionRoot {
    pub path: PathBuf,
    pub flat: bool,
}

/// The Pi session root for this environment. The workspace directory plays
/// the role of upstream's process cwd for Pi's settings: its
/// `.pi/settings.json` is consulted and a relative `sessionDir` resolves
/// against it. Relative `PI_CODING_AGENT_DIR` / `PI_CODING_AGENT_SESSION_DIR`
/// values resolve against the launch directory
/// ([`HistoryEnv::env_value_base_dir`], K16), else the workspace. A base that
/// is not an absolute path is ignored.
pub fn session_root(env: &HistoryEnv) -> Result<PiSessionRoot> {
    session_root_from(
        env.pi_coding_agent_dir.clone(),
        env.pi_coding_agent_session_dir.clone(),
        env.home_dir.clone(),
        env.workspace_base_dir(),
        env.env_value_base_dir(),
    )
}

fn session_root_from(
    agent_override: Option<PathBuf>,
    session_override: Option<PathBuf>,
    home_dir: Option<PathBuf>,
    cwd: Option<PathBuf>,
    env_base: Option<PathBuf>,
) -> Result<PiSessionRoot> {
    let base = cwd.as_deref();
    let env_base = env_base.as_deref();
    let agent_dir = if let Some(value) = agent_override {
        expand_path_with_home(value, home_dir.as_deref(), env_base)?
    } else {
        home_dir
            .clone()
            .ok_or_else(|| {
                AppError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Could not determine home directory",
                ))
            })?
            .join(".pi")
            .join("agent")
    };

    if let Some(value) = session_override {
        return Ok(PiSessionRoot {
            path: expand_path_with_home(value, home_dir.as_deref(), env_base)?,
            flat: true,
        });
    }

    let global_setting = configured_session_dir(&agent_dir.join("settings.json"));
    let project_setting = cwd
        .as_deref()
        .and_then(|cwd| configured_session_dir(&cwd.join(".pi/settings.json")));
    if let Some(Some(value)) = project_setting.or(global_setting) {
        return Ok(PiSessionRoot {
            path: expand_path_with_home(value, home_dir.as_deref(), base)?,
            flat: true,
        });
    }

    Ok(PiSessionRoot {
        path: agent_dir.join("sessions"),
        flat: false,
    })
}

fn configured_session_dir(path: &Path) -> Option<Option<PathBuf>> {
    let contents = std::fs::read_to_string(path).ok()?;
    let settings = serde_json::from_str::<Value>(&contents).ok()?;
    let value = settings.as_object()?.get("sessionDir")?;
    Some(
        value
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
    )
}

/// Expand `~`/`~/…` against the home directory and resolve relative paths
/// against `base` (the workspace; upstream used the process cwd). Without a
/// base a relative path is an error, as upstream's failing `current_dir()`.
fn expand_path_with_home(
    path: PathBuf,
    home_dir: Option<&Path>,
    base: Option<&Path>,
) -> Result<PathBuf> {
    let missing_home = || {
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine home directory",
        ))
    };
    let expanded = if path == Path::new("~") {
        home_dir.map(Path::to_path_buf).ok_or_else(missing_home)?
    } else if let Ok(rest) = path.strip_prefix("~/") {
        home_dir.ok_or_else(missing_home)?.join(rest)
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
                "Cannot resolve relative Pi path '{}' without a workspace directory",
                expanded.display()
            ),
        ))
    })?;
    Ok(base.join(expanded))
}

pub fn discover_files(root: &PiSessionRoot) -> Result<Vec<PathBuf>> {
    if !root.path.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    if root.flat {
        collect_jsonl(&root.path, &mut files)?;
    } else {
        for entry in read_dir(&root.path)? {
            let entry = entry?;
            let path = entry.path();
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
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    Ok(())
}

pub fn load_pi_conversations(
    env: &HistoryEnv,
    show_last: bool,
    debug_level: Option<DebugLevel>,
) -> Result<Vec<Conversation>> {
    let root = session_root(env)?;
    let files = discover_files(&root)?;
    let cached = super::cache::read_pi_cache(env, &root.path).unwrap_or_default();
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
                conversation.source = Source::Pi;
                conversation.session_id = entry.session_id.clone();
                conversation.cwd = Some(entry.project_path.clone());
                conversation.project_path = Some(entry.project_path.clone());
                conversation.project_name = Some(format_short_name_from_path(&entry.project_path));
                conversation
            });
        if conversation.is_none() {
            conversation = match process_conversation_file(path.clone(), modified, debug_level) {
                Ok(Some(conversation)) if conversation.source == Source::Pi => Some(conversation),
                Ok(_) => None,
                Err(error) => {
                    debug::warn(
                        debug_level,
                        &format!("Failed to parse Pi session {}: {error}", path.display()),
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
    super::cache::write_pi_cache(env, &root.path, updated_cache);
    conversations.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
    for (index, conversation) in conversations.iter_mut().enumerate() {
        conversation.index = index;
    }
    Ok(conversations)
}

pub fn delete_session(path: &Path) -> Result<()> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl")
        || !super::pi::is_pi_file(path)
    {
        return Err(AppError::SessionNotFound(path.display().to_string()));
    }
    std::fs::remove_file(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_directory_precedence_matches_pi() {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agent");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(
            agent.join("settings.json"),
            r#"{"sessionDir":"~/settings-sessions"}"#,
        )
        .unwrap();

        let settings = session_root_from(
            Some(agent.clone()),
            None,
            Some(home.path().to_path_buf()),
            None,
            None,
        )
        .unwrap();
        assert_eq!(settings.path, home.path().join("settings-sessions"));
        assert!(settings.flat);

        let project = home.path().join("project");
        std::fs::create_dir_all(project.join(".pi")).unwrap();
        std::fs::write(
            project.join(".pi/settings.json"),
            r#"{"sessionDir":"./project-sessions"}"#,
        )
        .unwrap();
        let project_settings = session_root_from(
            Some(agent.clone()),
            None,
            Some(home.path().to_path_buf()),
            Some(project.clone()),
            Some(project.clone()),
        )
        .unwrap();
        // Upstream resolved this against the process cwd; core resolves it
        // against the workspace directory (which also supplied the settings).
        assert_eq!(project_settings.path, project.join("project-sessions"));
        assert!(project_settings.flat);

        let environment = session_root_from(
            Some(agent),
            Some(PathBuf::from("~/environment-sessions")),
            Some(home.path().to_path_buf()),
            None,
            None,
        )
        .unwrap();
        assert_eq!(environment.path, home.path().join("environment-sessions"));
        assert!(environment.flat);
    }

    #[test]
    fn relative_environment_values_resolve_against_the_launch_directory() {
        // K16: PI_* values were set in the shell the GUI was started from;
        // with launch directory A and workspace B they resolve under A.
        let home = tempfile::tempdir().unwrap();
        let launch = home.path().join("launch");
        let workspace = home.path().join("workspace");
        let env = HistoryEnv::sandbox(home.path())
            .with_pi_session_dir("relative-sessions")
            .with_workspace(&workspace)
            .with_env_path_base(&launch);
        let root = session_root(&env).unwrap();
        assert_eq!(root.path, launch.join("relative-sessions"));

        let agent_env = HistoryEnv::sandbox(home.path())
            .with_pi_coding_agent_dir("relative-agent")
            .with_workspace(&workspace)
            .with_env_path_base(&launch);
        assert_eq!(
            session_root(&agent_env).unwrap().path,
            launch.join("relative-agent").join("sessions")
        );

        // Without a launch directory the workspace is the base (as before);
        // a relative launch directory is ignored.
        let mut without = env.clone();
        without.env_path_base = None;
        assert_eq!(
            session_root(&without).unwrap().path,
            workspace.join("relative-sessions")
        );
        let relative_base = env.clone().with_env_path_base("launch");
        assert_eq!(
            session_root(&relative_base).unwrap().path,
            workspace.join("relative-sessions")
        );

        // The workspace's own .pi/settings.json stays workspace-relative.
        std::fs::create_dir_all(workspace.join(".pi")).unwrap();
        std::fs::write(
            workspace.join(".pi/settings.json"),
            r#"{"sessionDir":"project-sessions"}"#,
        )
        .unwrap();
        let settings_env = HistoryEnv::sandbox(home.path())
            .with_workspace(&workspace)
            .with_env_path_base(&launch);
        assert_eq!(
            session_root(&settings_env).unwrap().path,
            workspace.join("project-sessions")
        );
    }

    #[test]
    fn relative_paths_need_a_workspace_and_resolve_against_it() {
        let home = tempfile::tempdir().unwrap();
        let workspace = home.path().join("workspace");
        let env = HistoryEnv::sandbox(home.path())
            .with_pi_session_dir("relative-sessions")
            .with_workspace(&workspace);
        let root = session_root(&env).unwrap();
        assert_eq!(root.path, workspace.join("relative-sessions"));
        assert!(root.flat);

        let mut no_workspace = env.clone();
        no_workspace.workspace_dir = None;
        assert!(session_root(&no_workspace).is_err());

        // A relative workspace would resolve against the process cwd (D2):
        // it is ignored, so the relative session dir cannot be resolved.
        let relative = env.clone().with_workspace("workspace");
        assert!(session_root(&relative).is_err());
        // A trailing separator does not change the base.
        let mut trailing = workspace.clone().into_os_string();
        trailing.push(std::path::MAIN_SEPARATOR_STR);
        let root = session_root(&env.clone().with_workspace(trailing)).unwrap();
        assert_eq!(root.path, workspace.join("relative-sessions"));
    }

    #[test]
    fn sandbox_default_root_is_under_home() {
        let home = tempfile::tempdir().unwrap();
        let root = session_root(&HistoryEnv::sandbox(home.path())).unwrap();
        assert_eq!(
            root.path,
            home.path().join(".pi").join("agent").join("sessions")
        );
        assert!(!root.flat);
    }

    #[test]
    fn discovers_flat_and_nested_roots_and_validates_headers() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("nested");
        let project = nested.join("--tmp-project--");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/v1.jsonl"),
            project.join("session.jsonl"),
        )
        .unwrap();
        std::fs::write(project.join("not-pi.jsonl"), "{\"type\":\"user\"}\n").unwrap();

        let nested_files = discover_files(&PiSessionRoot {
            path: nested,
            flat: false,
        })
        .unwrap();
        assert_eq!(nested_files.len(), 2);
        assert!(
            crate::history::pi::is_pi_file(&nested_files[0])
                || crate::history::pi::is_pi_file(&nested_files[1])
        );

        let flat = directory.path().join("flat");
        std::fs::create_dir_all(&flat).unwrap();
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/v2.jsonl"),
            flat.join("flat.jsonl"),
        )
        .unwrap();
        assert_eq!(
            discover_files(&PiSessionRoot {
                path: flat,
                flat: true,
            })
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn delete_removes_only_the_valid_pi_jsonl() {
        let directory = tempfile::tempdir().unwrap();
        let session = directory.path().join("session.jsonl");
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/v1.jsonl"),
            &session,
        )
        .unwrap();
        let sibling = directory.path().join("sibling.txt");
        std::fs::write(&sibling, "keep").unwrap();

        delete_session(&session).unwrap();
        assert!(!session.exists());
        assert!(sibling.exists());
    }

    #[cfg(unix)]
    #[test]
    fn nested_discovery_follows_symlinked_project_directories() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let root = directory.path().join("sessions");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/v3-branched.jsonl"),
            target.join("session.jsonl"),
        )
        .unwrap();
        symlink(&target, root.join("--linked--")).unwrap();

        let files = discover_files(&PiSessionRoot {
            path: root,
            flat: false,
        })
        .unwrap();
        assert_eq!(files.len(), 1);
        assert!(crate::history::pi::is_pi_file(&files[0]));
    }
}
