use crate::error::{AppError, Result};
use crate::semantic::embed::SemanticEmbedder;
use crate::semantic::types::DEFAULT_EMBEDDING_BATCH_SIZE;
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
    ///
    /// Guard rail (K13): in this crate's tests and in every `hermetic` build
    /// (the GUI's tests) it fails with [`HERMETIC_REFUSAL`] before touching
    /// `model_dir` or the network, also when a caller bypasses the default
    /// embedder factories.
    pub fn new_in(model_dir: PathBuf, show_download_progress: bool) -> Result<Self> {
        refuse_in_hermetic_builds()?;
        Self::load_in(model_dir, show_download_progress)
    }

    /// The loader behind [`FastembedEmbedder::new_in`], without its guard.
    fn load_in(model_dir: PathBuf, show_download_progress: bool) -> Result<Self> {
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
            InitOptionsUserDefined::new().with_intra_threads(1),
        )
        .map_err(to_config_error)?;
        Ok(Self { model })
    }
}

/// The production embedder in `model_dir`, quiet (no hf-hub progress bar
/// on stderr). Every default embedder (semantic worker, agent service,
/// semantic operations) goes through here, so in tests and `hermetic`
/// builds it refuses like [`FastembedEmbedder::new_in`].
pub(crate) fn quiet_embedder_in(
    model_dir: PathBuf,
) -> Result<Box<dyn crate::semantic::embed::SemanticEmbedder>> {
    Ok(Box::new(FastembedEmbedder::new_in(model_dir, false)?))
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
    FastembedEmbedder::load_in(model_dir, false)
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
    fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
        self.model
            .embed(
                prefixed_passages(passages),
                Some(DEFAULT_EMBEDDING_BATCH_SIZE),
            )
            .map_err(to_config_error)
    }

    fn embed_query(&mut self, query: &str) -> Result<Option<Vec<f32>>> {
        let embeddings = self
            .model
            .embed(
                vec![prefixed_query(query)],
                Some(DEFAULT_EMBEDDING_BATCH_SIZE),
            )
            .map_err(to_config_error)?;
        Ok(embeddings.first().cloned())
    }
}

pub fn prefixed_query(query: &str) -> String {
    format!("Represent this sentence for searching relevant passages: {query}")
}

pub fn prefixed_passages(passages: &[String]) -> Vec<String> {
    passages.to_vec()
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
        for progress in [false, true] {
            let error = match FastembedEmbedder::new_in(model_dir.clone(), progress) {
                Ok(_) => panic!("the real model must not load in tests"),
                Err(error) => error.to_string(),
            };
            assert!(error.contains(HERMETIC_REFUSAL), "{error}");
        }
        let error = match quiet_embedder_in(model_dir.clone()) {
            Ok(_) => panic!("the real model must not load in tests"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains(HERMETIC_REFUSAL), "{error}");
        assert!(!temp.path().join("semantic").exists(), "nothing created");
    }

    #[test]
    fn leaves_passages_unprefixed_for_fastembed() {
        assert_eq!(
            prefixed_passages(&["one".to_string(), "two".to_string()]),
            vec!["one".to_string(), "two".to_string()]
        );
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
