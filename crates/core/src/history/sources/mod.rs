//! The adapters of each source, and the registry of the instances the
//! environment configures.
//!
//! # Differences from upstream
//!
//! * **Roots from a [`HistoryEnv`].** [`Registry::from_env`] takes the env
//!   (the Claude projects root and the Pi/OMP session roots the history
//!   loader reads; the Pi root follows the workspace's `.pi/settings.json`)
//!   instead of reading the process environment. Like upstream's, a registry
//!   is built for each pass, so it follows a workspace change.
//! * **The CLI's cache dir.** A registry built from an env also carries the
//!   installed CLI's cache root ([`Registry::cli_cache`],
//!   [`HistoryEnv::cli_cache_root`]): a reconcile pass never removes old
//!   caches from a cache dir inside it (`store::legacy`).
//!   [`Registry::with_roots`] leaves it unset.

pub mod claude;
pub mod pi;

use super::adapter::{Adapter, FileId, Located, Stamp};
use super::model::{ConversationKey, SourceInstance, SourceKind};
use super::omp_loader::{self, OmpSessionRoot};
use super::pi_loader::{self, PiSessionRoot};
use crate::env::HistoryEnv;
use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// The adapters of every configured history root, in a fixed order: Claude,
/// Pi, OMP.
#[derive(Clone, Default)]
pub struct Registry {
    adapters: Vec<Arc<dyn Adapter>>,
    /// Fork: the installed CLI's cache root, see [`Self::cli_cache`].
    cli_cache: Option<PathBuf>,
}

impl Registry {
    /// The roots `env` selects (`CLAUDE_CONFIG_DIR`, the Pi and OMP
    /// settings), whether or not they exist (fork: from the [`HistoryEnv`],
    /// not the process environment), and the CLI's cache root.
    pub fn from_env(env: &HistoryEnv) -> Self {
        Self::with_roots(
            env.claude_projects_root().ok(),
            pi_loader::session_root(env).ok(),
            omp_loader::session_root(env).ok(),
        )
        .with_cli_cache(env.cli_cache_root())
    }

    /// Adapters for a Claude `projects` directory and Pi and OMP session
    /// roots.
    pub fn with_roots(
        claude: Option<PathBuf>,
        pi: Option<PiSessionRoot>,
        omp: Option<OmpSessionRoot>,
    ) -> Self {
        let mut adapters: Vec<Arc<dyn Adapter>> = Vec::new();
        if let Some(root) = claude {
            adapters.push(Arc::new(claude::ClaudeAdapter::new(root)));
        }
        if let Some(root) = pi {
            adapters.push(Arc::new(pi::PiAdapter::pi(root)));
        }
        if let Some(root) = omp {
            adapters.push(Arc::new(pi::PiAdapter::omp(root)));
        }
        Self {
            adapters,
            cli_cache: None,
        }
    }

    /// Fork: sets the installed CLI's cache root
    /// ([`HistoryEnv::cli_cache_root`]).
    pub fn with_cli_cache(mut self, root: Option<PathBuf>) -> Self {
        self.cli_cache = root;
        self
    }

    /// Fork: the installed CLI's cache root. A reconcile pass never removes
    /// old caches from a cache dir inside it, even when the env's cache root
    /// was pointed there.
    pub fn cli_cache(&self) -> Option<&Path> {
        self.cli_cache.as_deref()
    }

    pub fn adapters(&self) -> &[Arc<dyn Adapter>] {
        &self.adapters
    }

    /// Adapters of `kind`.
    pub fn of_kind(&self, kind: SourceKind) -> impl Iterator<Item = &Arc<dyn Adapter>> {
        self.adapters
            .iter()
            .filter(move |adapter| adapter.instance().kind == kind)
    }
}

/// `Located` for each of `paths`, an instance's session files, one per
/// conversation, stated in parallel. Files that cannot be stated are left
/// out.
fn locate_files(
    instance: &SourceInstance,
    paths: Vec<PathBuf>,
    scan_started: SystemTime,
) -> Vec<Located> {
    paths
        .into_par_iter()
        .filter_map(|path| locate_file(instance, path, scan_started))
        .collect()
}

fn locate_file(
    instance: &SourceInstance,
    path: PathBuf,
    scan_started: SystemTime,
) -> Option<Located> {
    let metadata = fs::metadata(&path).ok()?;
    let modified = metadata.modified().ok();
    let size = metadata.len();
    Some(Located {
        key: ConversationKey {
            instance: instance.clone(),
            native_id: native_id(&instance.root, &path),
        },
        stamp: Stamp::of(size, modified, scan_started),
        file_id: file_id(&metadata),
        path,
        size,
        modified,
    })
}

/// Native id of a session file: its path relative to the instance root.
fn native_id(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(unix)]
fn file_id(metadata: &fs::Metadata) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_id(_metadata: &fs::Metadata) -> Option<FileId> {
    None
}
