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
    let agent_dir = agent_dir_from(agent_override, home_dir.as_deref(), env_base)?;

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

/// Pi's agent directory: `PI_CODING_AGENT_DIR` (resolved like
/// [`session_root`] resolves it), else `<home>/.pi/agent`.
fn agent_dir_from(
    agent_override: Option<PathBuf>,
    home_dir: Option<&Path>,
    env_base: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(value) = agent_override {
        return expand_path_with_home(value, home_dir, env_base);
    }
    Ok(home_dir
        .ok_or_else(|| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not determine home directory",
            ))
        })?
        .join(".pi")
        .join("agent"))
}

/// Fork: the Pi settings files of `env` that name a `sessionDir`, each with
/// the directory it names, resolved as [`session_root`] resolves it (`~`
/// against the home, a relative value against the workspace): the agent
/// directory's `settings.json`, then the workspace's `.pi/settings.json`.
/// A file that is missing or not JSON, names no (or a blank) `sessionDir`,
/// or whose value cannot be resolved (relative, without a workspace) is
/// left out: [`session_root`] then does not use it either. Whichever of
/// them [`session_root`] picks (the workspace's wins), the GUI's QA
/// sandbox gate checks every one.
pub fn session_dir_settings(env: &HistoryEnv) -> Vec<(PathBuf, PathBuf)> {
    let home_dir = env.home_dir.as_deref();
    let cwd = env.workspace_base_dir();
    let env_base = env.env_value_base_dir();
    let global = agent_dir_from(
        env.pi_coding_agent_dir.clone(),
        home_dir,
        env_base.as_deref(),
    )
    .ok()
    .map(|agent_dir| agent_dir.join("settings.json"));
    let project = cwd
        .as_deref()
        .map(|cwd| cwd.join(".pi").join("settings.json"));
    global
        .into_iter()
        .chain(project)
        .filter_map(|file| {
            let value = configured_session_dir(&file)??;
            let dir = expand_path_with_home(value, home_dir, cwd.as_deref()).ok()?;
            Some((file, dir))
        })
        .collect()
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
            // The listing's file type saves a stat per directory; a symlink
            // is followed to see whether it leads to one.
            let is_dir = match entry.file_type() {
                Ok(kind) if !kind.is_symlink() => kind.is_dir(),
                _ => path.is_dir(),
            };
            if is_dir {
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

    /// Fork: the settings that can choose the session root, each resolved
    /// as `session_root` resolves it (the GUI's sandbox gate checks them).
    #[test]
    fn session_dir_settings_name_every_configured_directory() {
        let home = tempfile::tempdir().unwrap();
        let env = HistoryEnv::sandbox(home.path());
        assert!(session_dir_settings(&env).is_empty(), "no settings files");

        let agent = home.path().join(".pi").join("agent");
        std::fs::create_dir_all(&agent).unwrap();
        let global = agent.join("settings.json");
        std::fs::write(&global, r#"{"sessionDir":"~/global-sessions"}"#).unwrap();
        let named = home.path().join("global-sessions");
        assert_eq!(
            session_dir_settings(&env),
            vec![(global.clone(), named.clone())]
        );
        assert_eq!(session_root(&env).unwrap().path, named);

        // A relative value resolves against the workspace; without one it
        // names nothing (and `session_root` fails).
        std::fs::write(&global, r#"{"sessionDir":"relative"}"#).unwrap();
        assert!(session_dir_settings(&env).is_empty());
        assert!(session_root(&env).is_err());
        let workspace = home.path().join("workspace");
        std::fs::create_dir_all(workspace.join(".pi")).unwrap();
        let env = env.with_workspace(&workspace);
        assert_eq!(
            session_dir_settings(&env),
            vec![(global.clone(), workspace.join("relative"))]
        );

        // The workspace's own settings follow (and win in `session_root`).
        let project = workspace.join(".pi").join("settings.json");
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(
            &project,
            serde_json::json!({ "sessionDir": outside.path() }).to_string(),
        )
        .unwrap();
        assert_eq!(
            session_dir_settings(&env),
            vec![
                (global.clone(), workspace.join("relative")),
                (project.clone(), outside.path().to_path_buf()),
            ]
        );
        assert_eq!(session_root(&env).unwrap().path, outside.path());

        // Blank values and files that are not JSON name nothing.
        std::fs::write(&global, r#"{"sessionDir":"  "}"#).unwrap();
        std::fs::write(&project, "not json").unwrap();
        assert!(session_dir_settings(&env).is_empty());

        // PI_CODING_AGENT_DIR moves the agent directory's settings file.
        let other_agent = home.path().join("other-agent");
        std::fs::create_dir_all(&other_agent).unwrap();
        std::fs::write(
            other_agent.join("settings.json"),
            r#"{"sessionDir":"~/other-sessions"}"#,
        )
        .unwrap();
        assert_eq!(
            session_dir_settings(&env.with_pi_coding_agent_dir(&other_agent)),
            vec![(
                other_agent.join("settings.json"),
                home.path().join("other-sessions")
            )]
        );
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
