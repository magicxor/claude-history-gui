//! The fastembed embedder (upstream `semantic/fastembed.rs`).
//!
//! Differences from upstream:
//!
//! * The model is loaded from a directory the caller passes
//!   ([`FastembedEmbedder::new_in`], normally
//!   [`crate::env::HistoryEnv::semantic_model_dir`]), fetched with hf-hub
//!   directly (see `new_in`), and refused in tests and `hermetic` builds
//!   ([`HERMETIC_REFUSAL`]).
//! * [`EmbedPriority`] (upstream 8322afe) only sizes the inference thread
//!   pool here. Upstream's `new_quiet(Background)` also lowered the
//!   scheduling priority of the thread making the embedder; the GUI's one
//!   shared model may be made on the agent worker or a Tools thread, so the
//!   semantic worker lowers its own thread instead
//!   ([`lower_background_thread_priority`]), and so does the store's
//!   reconcile pool (upstream ran reconcile in a child process at nice 10).
//!   Lowering also covers Windows (`THREAD_PRIORITY_BELOW_NORMAL`), which
//!   upstream left out, and is skipped in tests and `hermetic` builds.

use crate::error::{AppError, Result};
use crate::semantic::embed::SemanticEmbedder;
use crate::semantic::types::MODEL_BATCH_SIZE;
use fastembed::{
    EmbeddingModel, InitOptionsUserDefined, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};
use hf_hub::Cache;
use hf_hub::api::sync::{ApiBuilder, ApiRepo};
use std::path::PathBuf;

#[cfg(feature = "release-dynamic-ort")]
use std::{path::Path, sync::OnceLock};

/// The embedding model (upstream: `EmbeddingModel::BGESmallENV15`).
const MODEL: EmbeddingModel = EmbeddingModel::BGESmallENV15;
/// The Hugging Face endpoint. fastembed honored `HF_ENDPOINT`; core does
/// not read the process environment outside [`crate::env::HistoryEnv`].
const HF_ENDPOINT: &str = "https://huggingface.co";

pub struct FastembedEmbedder {
    model: TextEmbedding,
}

/// How much of the machine embedding may use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbedPriority {
    /// A command the person is waiting on: every core.
    Foreground,
    /// Indexing behind the list: half the cores (fork: at a lower
    /// scheduling priority where the semantic worker runs it, see the
    /// module notes), so the list and lexical search stay responsive.
    Background,
}

impl EmbedPriority {
    fn threads(self) -> usize {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        match self {
            Self::Foreground => cores,
            Self::Background => (cores / 2).max(1),
        }
    }
}

impl FastembedEmbedder {
    /// Load (downloading on first use) the BGE-small model into `model_dir`
    /// (normally [`crate::env::HistoryEnv::semantic_model_dir`]).
    ///
    /// The files are fetched with hf-hub directly instead of fastembed's
    /// `TextEmbedding::try_new`, which let `HF_HOME` relocate the model
    /// (so `clear_semantic_cache` missed it), honored `HF_ENDPOINT`, read
    /// the real `~/.cache/huggingface/token` (sending it as a bearer token)
    /// and panicked without a home directory. The cache layout under
    /// `model_dir` (`models--Xenova--bge-small-en-v1.5/...`) is unchanged,
    /// so earlier downloads are reused; the model is built exactly as
    /// `try_new` builds it (same files, pooling, quantization and output).
    ///
    /// GUI callers must pass `show_download_progress = false`: `true` makes
    /// hf-hub draw an indicatif progress bar on stderr (upstream CLI only).
    /// `priority` sizes the inference thread pool; it never changes the
    /// calling thread's scheduling priority (see the module notes).
    ///
    /// Guard rail (K13): in this crate's tests and in every `hermetic` build
    /// (the GUI's tests) it fails with [`HERMETIC_REFUSAL`] before touching
    /// `model_dir` or the network, also when a caller bypasses the default
    /// embedder factories.
    pub fn new_in(
        model_dir: PathBuf,
        show_download_progress: bool,
        priority: EmbedPriority,
    ) -> Result<Self> {
        refuse_in_hermetic_builds()?;
        Self::load_in(model_dir, show_download_progress, priority)
    }

    /// The loader behind [`FastembedEmbedder::new_in`], without its guard.
    fn load_in(
        model_dir: PathBuf,
        show_download_progress: bool,
        priority: EmbedPriority,
    ) -> Result<Self> {
        init_onnx_runtime()?;
        let info = TextEmbedding::get_model_info(&MODEL).map_err(to_config_error)?;
        if !info.additional_files.is_empty() {
            // External weight files must sit next to an on-disk model; the
            // in-memory session below cannot load them.
            return Err(AppError::ConfigError(format!(
                "{MODEL:?} needs external model files, which are not supported"
            )));
        }
        let repo = model_api_builder(model_dir, show_download_progress)
            .build()
            .map_err(to_config_error)?
            .model(info.model_code.clone());
        let files = read_model_files(&repo, &info.model_file)?;
        let mut user_defined = UserDefinedEmbeddingModel::new(files.onnx, files.tokenizer)
            .with_quantization(TextEmbedding::get_quantization_mode(&MODEL));
        user_defined.pooling = TextEmbedding::get_default_pooling_method(&MODEL);
        user_defined.output_key = info.output_key.clone();
        let model = TextEmbedding::try_new_from_user_defined(
            user_defined,
            InitOptionsUserDefined::new().with_intra_threads(priority.threads()),
        )
        .map_err(to_config_error)?;
        Ok(Self { model })
    }
}

/// The production embedder in `model_dir`, quiet (no hf-hub progress bar
/// on stderr). Every default embedder (semantic worker, agent service,
/// semantic operations) goes through here, so in tests and `hermetic`
/// builds it refuses like [`FastembedEmbedder::new_in`]. Upstream's
/// `new_quiet(priority)`, without lowering the calling thread's priority.
pub(crate) fn quiet_embedder_in(
    model_dir: PathBuf,
    priority: EmbedPriority,
) -> Result<Box<dyn crate::semantic::embed::SemanticEmbedder>> {
    Ok(Box::new(FastembedEmbedder::new_in(
        model_dir, false, priority,
    )?))
}

/// The error of [`FastembedEmbedder::new_in`] in tests and `hermetic`
/// builds.
pub const HERMETIC_REFUSAL: &str =
    "tests must inject a semantic embedder factory; the real model is never loaded in tests";

#[cfg(not(any(test, feature = "hermetic")))]
fn refuse_in_hermetic_builds() -> Result<()> {
    Ok(())
}

/// Guard rail: tests (and every `hermetic` build) must inject an embedder,
/// so no test loads (or downloads) the real model.
#[cfg(any(test, feature = "hermetic"))]
fn refuse_in_hermetic_builds() -> Result<()> {
    Err(AppError::ConfigError(HERMETIC_REFUSAL.to_string()))
}

/// The real model despite the guard, only for the opt-in network tests
/// (`#[ignore]` plus `CLAUDE_HISTORY_NETWORK_TESTS=1`).
#[cfg(test)]
pub(crate) fn real_model_for_network_test(model_dir: PathBuf) -> Result<FastembedEmbedder> {
    FastembedEmbedder::load_in(model_dir, false, EmbedPriority::Background)
}

/// The hf-hub API over the cache in `model_dir` only: the fixed public
/// endpoint and no token (`with_token(None)` drops the token `from_cache`
/// looks for beside `model_dir`, inside core's own cache namespace).
fn model_api_builder(model_dir: PathBuf, show_download_progress: bool) -> ApiBuilder {
    ApiBuilder::from_cache(Cache::new(model_dir))
        .with_token(None)
        .with_endpoint(HF_ENDPOINT.to_string())
        .with_progress(show_download_progress)
}

struct ModelFiles {
    onnx: Vec<u8>,
    tokenizer: TokenizerFiles,
}

/// The files `TextEmbedding::try_new` reads (the model and the four
/// tokenizer files), from the cache or downloaded into it.
fn read_model_files(repo: &ApiRepo, model_file: &str) -> Result<ModelFiles> {
    let read = |file: &str| -> Result<Vec<u8>> {
        let path = repo.get(file).map_err(|error| {
            AppError::ConfigError(format!("Failed to retrieve {file}: {error}"))
        })?;
        Ok(std::fs::read(path)?)
    };
    Ok(ModelFiles {
        onnx: read(model_file)?,
        tokenizer: TokenizerFiles {
            tokenizer_file: read("tokenizer.json")?,
            config_file: read("config.json")?,
            special_tokens_map_file: read("special_tokens_map.json")?,
            tokenizer_config_file: read("tokenizer_config.json")?,
        },
    })
}

impl SemanticEmbedder for FastembedEmbedder {
    /// Embeds `passages` in small batches of similar length: a batch is
    /// padded to its longest passage, so mixing lengths wastes inference on
    /// padding, and small batches keep the activation memory small.
    fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
        let order = length_order(passages);
        let sorted = order
            .iter()
            .map(|&index| passages[index].as_str())
            .collect::<Vec<_>>();
        let embeddings = self
            .model
            .embed(prefixed_passages(&sorted), Some(MODEL_BATCH_SIZE))
            .map_err(to_config_error)?;
        Ok(unsort(&order, embeddings))
    }

    fn embed_query(&mut self, query: &str) -> Result<Option<Vec<f32>>> {
        let embeddings = self
            .model
            .embed(vec![prefixed_query(query)], Some(MODEL_BATCH_SIZE))
            .map_err(to_config_error)?;
        Ok(embeddings.first().cloned())
    }
}

/// Indices of `passages` from shortest to longest.
fn length_order(passages: &[String]) -> Vec<usize> {
    let mut order = (0..passages.len()).collect::<Vec<_>>();
    order.sort_by_key(|&index| passages[index].len());
    order
}

/// Puts `values`, computed for the passages in `order`, back in passage
/// order.
fn unsort<T: Default>(order: &[usize], values: Vec<T>) -> Vec<T> {
    let mut out = (0..values.len()).map(|_| T::default()).collect::<Vec<_>>();
    for (&index, value) in order.iter().zip(values) {
        out[index] = value;
    }
    out
}

/// Lowers the calling thread's scheduling priority for background work
/// (the semantic worker, the store's reconcile pool), with
/// [`lower_current_thread_priority`].
///
/// Guard rail: in this crate's tests and in `hermetic` builds (the GUI's
/// tests) the thread keeps its priority. A worker below the test threads'
/// priority could wait seconds for a core on a busy machine and make
/// timing-bound tests flaky; upstream's tests never lowered one either
/// (only the real embedder's constructor did).
pub(crate) fn lower_background_thread_priority() {
    if !cfg!(any(test, feature = "hermetic")) {
        lower_current_thread_priority();
    }
}

/// Lowers the scheduling priority of the calling thread: the utility QoS
/// class on macOS, nice 10 on Linux, and (fork) `THREAD_PRIORITY_BELOW_NORMAL`
/// on Windows. Threads the calling thread creates afterwards inherit it on
/// Linux only.
fn lower_current_thread_priority() {
    #[cfg(target_os = "macos")]
    // SAFETY: changes only the calling thread's QoS class.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
    #[cfg(target_os = "linux")]
    // SAFETY: changes only the calling thread's nice value; Linux applies
    // PRIO_PROCESS with a thread id to that thread alone.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 10);
    }
    #[cfg(windows)]
    // SAFETY: `GetCurrentThread` is the calling thread's pseudo handle, so
    // only that thread's priority changes. Below normal, not
    // THREAD_MODE_BACKGROUND_BEGIN, which would also lower the priority of
    // the thread's file I/O (the embedding cache writes).
    unsafe {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
        };
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

pub fn prefixed_query(query: &str) -> String {
    format!("Represent this sentence for searching relevant passages: {query}")
}

pub fn prefixed_passages(passages: &[&str]) -> Vec<String> {
    passages
        .iter()
        .map(|passage| (*passage).to_owned())
        .collect()
}

#[cfg(feature = "release-dynamic-ort")]
fn init_onnx_runtime() -> Result<()> {
    static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    match INIT.get_or_init(initialize_bundled_onnx_runtime) {
        Ok(()) => Ok(()),
        Err(message) => Err(AppError::ConfigError(message.clone())),
    }
}

#[cfg(not(feature = "release-dynamic-ort"))]
fn init_onnx_runtime() -> Result<()> {
    Ok(())
}

#[cfg(feature = "release-dynamic-ort")]
fn initialize_bundled_onnx_runtime() -> std::result::Result<(), String> {
    let path = bundled_onnx_runtime_path().ok_or_else(missing_runtime_message)?;
    let builder = ort::init_from(&path).map_err(|error| {
        format!(
            "Failed to load bundled ONNX Runtime from {}: {error}",
            path.display()
        )
    })?;
    if !builder.commit() {
        return Err(format!(
            "Failed to initialize bundled ONNX Runtime from {}: an ONNX Runtime environment is already configured",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(feature = "release-dynamic-ort")]
fn missing_runtime_message() -> String {
    format!(
        "Bundled ONNX Runtime library was not found beside the executable; expected a compatible {}",
        runtime_library_name()
    )
}

#[cfg(feature = "release-dynamic-ort")]
fn bundled_onnx_runtime_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    bundled_onnx_runtime_path_in_dir(exe.parent()?)
}

#[cfg(feature = "release-dynamic-ort")]
fn bundled_onnx_runtime_path_in_dir(dir: &Path) -> Option<PathBuf> {
    onnx_runtime_candidates(dir)
        .into_iter()
        .find(|candidate| candidate.is_file())
}

#[cfg(feature = "release-dynamic-ort")]
fn onnx_runtime_candidates(dir: &Path) -> Vec<PathBuf> {
    let name = runtime_library_name();
    let lib_dir = dir.join("lib");
    let mut candidates = vec![dir.join(name), lib_dir.join(name)];

    for library_dir in [dir, lib_dir.as_path()] {
        let Ok(entries) = std::fs::read_dir(library_dir) else {
            continue;
        };
        let mut versioned = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| is_versioned_runtime_name(path, name))
            .collect::<Vec<_>>();
        versioned.sort();
        candidates.extend(versioned);
    }
    candidates
}

#[cfg(feature = "release-dynamic-ort")]
fn runtime_library_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "libonnxruntime.dylib",
        "windows" => "onnxruntime.dll",
        _ => "libonnxruntime.so",
    }
}

#[cfg(feature = "release-dynamic-ort")]
fn is_versioned_runtime_name(path: &Path, unversioned_name: &str) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    match unversioned_name {
        "libonnxruntime.dylib" => {
            file_name.starts_with("libonnxruntime.")
                && file_name.ends_with(".dylib")
                && file_name != unversioned_name
        }
        "onnxruntime.dll" => false,
        _ => file_name.starts_with("libonnxruntime.so.") && file_name != unversioned_name,
    }
}

/// Load the bundled ONNX Runtime library now (a no-op without the
/// `release-dynamic-ort` feature). Release verify steps call this, so a missing
/// or incompatible library fails before the first semantic search does.
pub fn verify_onnx_runtime() -> Result<()> {
    init_onnx_runtime()
}

fn to_config_error(err: impl std::fmt::Display) -> AppError {
    AppError::ConfigError(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_query_for_fastembed() {
        assert_eq!(
            prefixed_query("rust cache"),
            "Represent this sentence for searching relevant passages: rust cache"
        );
    }

    #[test]
    fn new_in_refuses_in_tests_before_touching_the_model_dir() {
        // K13: the GUI loads its shared embedder with exactly this call
        // (DESIGN §7.6); a test that reaches it must not download.
        let temp = tempfile::tempdir().unwrap();
        let model_dir = temp.path().join("semantic").join("fastembed");
        for priority in [EmbedPriority::Foreground, EmbedPriority::Background] {
            for progress in [false, true] {
                let error = match FastembedEmbedder::new_in(model_dir.clone(), progress, priority) {
                    Ok(_) => panic!("the real model must not load in tests"),
                    Err(error) => error.to_string(),
                };
                assert!(error.contains(HERMETIC_REFUSAL), "{error}");
            }
            let error = match quiet_embedder_in(model_dir.clone(), priority) {
                Ok(_) => panic!("the real model must not load in tests"),
                Err(error) => error.to_string(),
            };
            assert!(error.contains(HERMETIC_REFUSAL), "{error}");
        }
        assert!(!temp.path().join("semantic").exists(), "nothing created");
    }

    #[test]
    fn leaves_passages_unprefixed_for_fastembed() {
        assert_eq!(
            prefixed_passages(&["one", "two"]),
            vec!["one".to_string(), "two".to_string()]
        );
    }

    #[test]
    fn length_order_round_trips() {
        let passages = vec!["ccc".to_string(), "a".to_string(), "bb".to_string()];
        let order = length_order(&passages);
        assert_eq!(order, vec![1, 2, 0]);
        let computed = order.iter().map(|&index| passages[index].len()).collect();
        assert_eq!(unsort(&order, computed), vec![3, 1, 2]);
    }

    #[test]
    fn background_embedding_uses_half_the_cores() {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        assert_eq!(EmbedPriority::Foreground.threads(), cores);
        assert_eq!(EmbedPriority::Background.threads(), (cores / 2).max(1));
    }

    /// The calling thread's scheduling priority as the OS reports it: its
    /// nice value.
    #[cfg(target_os = "linux")]
    fn current_thread_priority() -> i32 {
        // SAFETY: reads the calling thread's nice value.
        unsafe {
            let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
            libc::getpriority(libc::PRIO_PROCESS, tid)
        }
    }

    /// The calling thread's scheduling priority as the OS reports it.
    #[cfg(windows)]
    fn current_thread_priority() -> i32 {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadPriority};
        // SAFETY: reads the calling thread's priority through its pseudo
        // handle.
        unsafe { GetThreadPriority(GetCurrentThread()) }
    }

    /// Not read on other systems.
    #[cfg(not(any(target_os = "linux", windows)))]
    fn current_thread_priority() -> i32 {
        0
    }

    /// Fork: the helper lowers only the calling thread, on Windows too. It
    /// is called directly: the background call sites skip it in tests.
    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn lowering_the_priority_changes_only_the_calling_thread() {
        let before = current_thread_priority();
        let lowered = std::thread::spawn(|| {
            lower_current_thread_priority();
            current_thread_priority()
        })
        .join()
        .unwrap();
        #[cfg(target_os = "linux")]
        assert!(lowered >= 10, "nice {lowered}");
        #[cfg(windows)]
        assert_eq!(
            lowered,
            windows_sys::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL
        );
        assert_eq!(
            current_thread_priority(),
            before,
            "this thread is unchanged"
        );
    }

    #[test]
    fn background_threads_keep_their_priority_in_tests() {
        let (before, after) = std::thread::spawn(|| {
            let before = current_thread_priority();
            lower_background_thread_priority();
            (before, current_thread_priority())
        })
        .join()
        .unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn model_api_uses_only_the_model_dir_cache_the_public_endpoint_and_no_token() {
        // fastembed's try_new read HF_HOME / HF_ENDPOINT and the real
        // ~/.cache/huggingface/token. The builder must not depend on any of
        // them; even a token placed where hf-hub looks for one beside the
        // cache is ignored.
        let temp = tempfile::tempdir().unwrap();
        let semantic = temp.path().join("semantic");
        let model_dir = semantic.join("fastembed");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(semantic.join("token"), "hf_secret_token").unwrap();

        let debug = format!("{:?}", model_api_builder(model_dir.clone(), false));

        assert!(debug.contains("token: None"), "{debug}");
        assert!(!debug.contains("hf_secret_token"), "{debug}");
        assert!(debug.contains(&format!("path: {model_dir:?}")), "{debug}");
        assert!(
            debug.contains("endpoint: \"https://huggingface.co\""),
            "{debug}"
        );
        assert!(debug.contains("progress: false"), "{debug}");
    }

    #[test]
    fn model_files_are_read_from_the_model_dir_cache_layout() {
        // The layout fastembed's try_new used under `with_cache_dir`, so a
        // model downloaded before keeps being used (no network here: every
        // file is a cache hit).
        let temp = tempfile::tempdir().unwrap();
        let model_dir = temp.path().join("fastembed");
        let info = TextEmbedding::get_model_info(&MODEL).unwrap();
        let repo_dir = model_dir.join(format!("models--{}", info.model_code.replace('/', "--")));
        assert!(repo_dir.ends_with("models--Xenova--bge-small-en-v1.5"));
        std::fs::create_dir_all(repo_dir.join("refs")).unwrap();
        std::fs::write(repo_dir.join("refs").join("main"), "abc123").unwrap();
        let snapshot = repo_dir.join("snapshots").join("abc123");
        let files = [
            info.model_file.as_str(),
            "tokenizer.json",
            "config.json",
            "special_tokens_map.json",
            "tokenizer_config.json",
        ];
        for file in files {
            let path = snapshot.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, format!("<{file}>")).unwrap();
        }

        let repo = model_api_builder(model_dir, false)
            .build()
            .unwrap()
            .model(info.model_code.clone());
        let read = read_model_files(&repo, &info.model_file).unwrap();

        assert_eq!(read.onnx, b"<onnx/model.onnx>");
        assert_eq!(read.tokenizer.tokenizer_file, b"<tokenizer.json>");
        assert_eq!(read.tokenizer.config_file, b"<config.json>");
        assert_eq!(
            read.tokenizer.special_tokens_map_file,
            b"<special_tokens_map.json>"
        );
        assert_eq!(
            read.tokenizer.tokenizer_config_file,
            b"<tokenizer_config.json>"
        );
        // The BGE-small model needs no external weight files.
        assert!(info.additional_files.is_empty());
    }

    #[cfg(not(feature = "release-dynamic-ort"))]
    #[test]
    fn non_release_runtime_initialization_is_a_noop() {
        assert!(init_onnx_runtime().is_ok());
    }

    #[cfg(feature = "release-dynamic-ort")]
    #[test]
    fn runtime_path_uses_the_host_library_name() {
        let temp = tempfile::tempdir().unwrap();
        let lib_dir = temp.path().join("lib");
        std::fs::create_dir(&lib_dir).unwrap();
        std::fs::write(lib_dir.join("libonnxruntime.dylib"), b"wrong host").unwrap();
        std::fs::write(lib_dir.join(runtime_library_name()), b"runtime").unwrap();

        assert_eq!(
            bundled_onnx_runtime_path_in_dir(temp.path()),
            Some(lib_dir.join(runtime_library_name()))
        );
    }

    #[cfg(feature = "release-dynamic-ort")]
    #[test]
    fn missing_runtime_error_names_the_expected_library() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(bundled_onnx_runtime_path_in_dir(temp.path()), None);
        let message = missing_runtime_message();
        assert!(message.starts_with(
            "Bundled ONNX Runtime library was not found beside the executable; expected a compatible "
        ));
        assert!(message.ends_with(runtime_library_name()));
    }

    #[cfg(feature = "release-dynamic-ort")]
    #[test]
    fn versioned_names_follow_each_host_convention() {
        let check = |name: &str, base: &str| is_versioned_runtime_name(Path::new(name), base);
        // macOS: libonnxruntime.<ver>.dylib, never the plain name.
        assert!(check("libonnxruntime.1.24.2.dylib", "libonnxruntime.dylib"));
        assert!(!check("libonnxruntime.dylib", "libonnxruntime.dylib"));
        assert!(!check("libonnxruntime.so.1", "libonnxruntime.dylib"));
        // Linux: libonnxruntime.so.<ver>.
        assert!(check("libonnxruntime.so.1.24.2", "libonnxruntime.so"));
        assert!(!check("libonnxruntime.so", "libonnxruntime.so"));
        assert!(!check("libother.so.1", "libonnxruntime.so"));
        // Windows has no versioned fallback.
        assert!(!check("onnxruntime.1.dll", "onnxruntime.dll"));
    }

    #[cfg(all(feature = "release-dynamic-ort", not(windows)))]
    #[test]
    fn runtime_path_falls_back_to_a_versioned_library() {
        let temp = tempfile::tempdir().unwrap();
        let lib_dir = temp.path().join("lib");
        std::fs::create_dir(&lib_dir).unwrap();
        let versioned_name = if runtime_library_name() == "libonnxruntime.dylib" {
            "libonnxruntime.1.24.2.dylib"
        } else {
            "libonnxruntime.so.1.24.2"
        };
        let versioned = lib_dir.join(versioned_name);
        std::fs::write(&versioned, b"runtime").unwrap();

        assert_eq!(
            bundled_onnx_runtime_path_in_dir(temp.path()),
            Some(versioned)
        );
    }
}
