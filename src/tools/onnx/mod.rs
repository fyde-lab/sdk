//! ONNX Runtime sessions and Hugging Face tokenizers for the on-device models
//! (fetched by `tools::models`). Only translates their errors: what to run,
//! and on which inputs, is up to the domain using them.
//!
//! Most targets link ONNX Runtime statically. On the others
//! (`cfg(ort_load_dynamic)`, see `build.rs`), the app ships its shared library
//! and [`session`] loads it first: from `ORT_DYLIB_PATH` when set, else by its
//! bare file name from the library search path.

use std::path::Path;

use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use tokenizers::Tokenizer;

use crate::error::{ErrorContext, ErrorKind, Result};

/// Loads the ONNX model at `path` into a session running on `threads` threads.
pub(crate) fn session(path: &Path, threads: usize) -> Result<Session> {
    load_runtime()?;

    Session::builder()
        .context("failed to create an ONNX Runtime session builder")?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(ort::Error::from)
        .context("failed to set the ONNX graph optimization level")?
        .with_intra_threads(threads)
        .map_err(ort::Error::from)
        .context("failed to set the ONNX Runtime thread count")?
        .commit_from_file(path)
        .with_context(|| format!("failed to load the ONNX model {}", path.display()))
}

/// Loads the Hugging Face `tokenizer.json` at `path`.
pub(crate) fn tokenizer(path: &Path) -> Result<Tokenizer> {
    Tokenizer::from_file(path)
        .map_err(ErrorKind::Tokenizer)
        .with_context(|| format!("failed to load the tokenizer {}", path.display()))
}

/// Loads the shared ONNX Runtime library, once: `ort` would otherwise load it
/// lazily on its first call and panic if it's missing.
#[cfg(ort_load_dynamic)]
fn load_runtime() -> Result<()> {
    let path = runtime_library_path(std::env::var_os("ORT_DYLIB_PATH"));

    // `false` when an earlier call already committed the environment.
    let _committed = ort::init_from(&path)
        .map_err(ErrorKind::OnnxRuntimeLoad)
        .with_context(|| format!("failed to load the ONNX Runtime library {}", path.display()))?
        .commit();

    Ok(())
}

#[cfg(not(ort_load_dynamic))]
fn load_runtime() -> Result<()> {
    Ok(())
}

/// `ORT_DYLIB_PATH`'s value when set, else the library's file name on this
/// platform (the same defaults `ort` itself falls back to).
#[cfg(any(test, ort_load_dynamic))]
fn runtime_library_path(env_override: Option<std::ffi::OsString>) -> std::path::PathBuf {
    match env_override {
        Some(path) if !path.is_empty() => path.into(),
        _ if cfg!(target_os = "windows") => "onnxruntime.dll".into(),
        _ if cfg!(target_vendor = "apple") => "libonnxruntime.dylib".into(),
        _ => "libonnxruntime.so".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;
    use ort::value::Tensor;

    /// A minimal ONNX model, hand-encoded with `prost`: a single `Identity`
    /// node from a float tensor `x` to `y`.
    mod identity_model {
        #[derive(Clone, PartialEq, prost::Message)]
        pub(super) struct ModelProto {
            #[prost(int64, tag = "1")]
            pub(super) ir_version: i64,
            #[prost(message, optional, tag = "7")]
            pub(super) graph: Option<GraphProto>,
            #[prost(message, repeated, tag = "8")]
            pub(super) opset_import: Vec<OperatorSetIdProto>,
        }

        #[derive(Clone, PartialEq, prost::Message)]
        pub(super) struct OperatorSetIdProto {
            #[prost(string, tag = "1")]
            pub(super) domain: String,
            #[prost(int64, tag = "2")]
            pub(super) version: i64,
        }

        #[derive(Clone, PartialEq, prost::Message)]
        pub(super) struct GraphProto {
            #[prost(message, repeated, tag = "1")]
            pub(super) node: Vec<NodeProto>,
            #[prost(string, tag = "2")]
            pub(super) name: String,
            #[prost(message, repeated, tag = "11")]
            pub(super) input: Vec<ValueInfoProto>,
            #[prost(message, repeated, tag = "12")]
            pub(super) output: Vec<ValueInfoProto>,
        }

        #[derive(Clone, PartialEq, prost::Message)]
        pub(super) struct NodeProto {
            #[prost(string, repeated, tag = "1")]
            pub(super) input: Vec<String>,
            #[prost(string, repeated, tag = "2")]
            pub(super) output: Vec<String>,
            #[prost(string, tag = "4")]
            pub(super) op_type: String,
        }

        #[derive(Clone, PartialEq, prost::Message)]
        pub(super) struct ValueInfoProto {
            #[prost(string, tag = "1")]
            pub(super) name: String,
            #[prost(message, optional, tag = "2")]
            pub(super) r#type: Option<TypeProto>,
        }

        #[derive(Clone, PartialEq, prost::Message)]
        pub(super) struct TypeProto {
            #[prost(message, optional, tag = "1")]
            pub(super) tensor_type: Option<TensorTypeProto>,
        }

        #[derive(Clone, PartialEq, prost::Message)]
        pub(super) struct TensorTypeProto {
            /// `TensorProto.DataType`: 1 is `FLOAT`.
            #[prost(int32, tag = "1")]
            pub(super) elem_type: i32,
        }

        pub(super) fn encode() -> Vec<u8> {
            let float = |name: &str| ValueInfoProto {
                name: name.into(),
                r#type: Some(TypeProto {
                    tensor_type: Some(TensorTypeProto { elem_type: 1 }),
                }),
            };
            let model = ModelProto {
                ir_version: 8,
                opset_import: vec![OperatorSetIdProto {
                    domain: String::new(),
                    version: 17,
                }],
                graph: Some(GraphProto {
                    name: "identity".into(),
                    node: vec![NodeProto {
                        input: vec!["x".into()],
                        output: vec!["y".into()],
                        op_type: "Identity".into(),
                    }],
                    input: vec![float("x")],
                    output: vec![float("y")],
                }),
            };

            prost::Message::encode_to_vec(&model)
        }
    }

    fn write_file(dir: &tempfile::TempDir, name: &str, content: &[u8]) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn session_loads_and_runs_a_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(&dir, "identity.onnx", &identity_model::encode());

        let mut session = session(&path, 1).unwrap();
        let input = Tensor::from_array(([3usize], vec![1.0_f32, 2.0, 3.0])).unwrap();
        let outputs = session.run(ort::inputs!["x" => input]).unwrap();
        let (shape, values) = outputs["y"].try_extract_tensor::<f32>().unwrap();

        assert_eq!(&**shape, &[3]);
        assert_eq!(values, &[1.0, 2.0, 3.0]);
    }

    #[test]
    fn session_fails_with_context_on_a_corrupt_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(&dir, "corrupt.onnx", b"definitely not a protobuf model");

        let err = session(&path, 1).unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelLoadFailed);
        assert!(err.has_context());
        assert!(err.to_string().contains("corrupt.onnx"), "{err}");
    }

    #[test]
    fn session_fails_with_context_on_a_missing_model() {
        let dir = tempfile::tempdir().unwrap();

        let err = session(&dir.path().join("missing.onnx"), 1).unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelLoadFailed);
        assert!(err.has_context());
    }

    #[test]
    fn tokenizer_loads_a_tokenizer_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(
            &dir,
            "tokenizer.json",
            br#"{
                "version": "1.0",
                "truncation": null,
                "padding": null,
                "added_tokens": [],
                "normalizer": null,
                "pre_tokenizer": {"type": "Whitespace"},
                "post_processor": null,
                "decoder": null,
                "model": {
                    "type": "WordLevel",
                    "vocab": {"[UNK]": 0, "hello": 1, "world": 2},
                    "unk_token": "[UNK]"
                }
            }"#,
        );

        let tokenizer = tokenizer(&path).unwrap();
        let encoding = tokenizer.encode("hello world again", false).unwrap();

        assert_eq!(encoding.get_ids(), &[1, 2, 0]);
    }

    #[test]
    fn tokenizer_fails_with_context_on_a_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(&dir, "tokenizer.json", b"{ not json");

        let err = tokenizer(&path).unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelLoadFailed);
        assert!(err.has_context());
        assert!(err.to_string().contains("tokenizer.json"), "{err}");
    }

    #[test]
    fn runtime_library_path_prefers_the_environment_override() {
        let path = runtime_library_path(Some("/opt/ort/libonnxruntime.so.1.28.0".into()));

        assert_eq!(path, Path::new("/opt/ort/libonnxruntime.so.1.28.0"));
    }

    #[test]
    fn runtime_library_path_defaults_to_the_platform_file_name() {
        let expected = if cfg!(target_os = "windows") {
            "onnxruntime.dll"
        } else if cfg!(target_vendor = "apple") {
            "libonnxruntime.dylib"
        } else {
            "libonnxruntime.so"
        };

        assert_eq!(runtime_library_path(None), Path::new(expected));
        assert_eq!(runtime_library_path(Some("".into())), Path::new(expected));
    }

    /// Set to run [`missing_runtime_library_child`]'s body: loading the
    /// runtime is once per process, so the failure needs a fresh one.
    #[cfg(ort_load_dynamic)]
    const MISSING_RUNTIME_CHILD_ENV: &str = "FYDE_TEST_MISSING_ONNX_RUNTIME";

    #[cfg(ort_load_dynamic)]
    #[test]
    fn session_fails_with_context_when_the_runtime_library_is_missing() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tools::onnx::tests::missing_runtime_library_child",
                "--include-ignored",
                "--test-threads=1",
            ])
            .env(MISSING_RUNTIME_CHILD_ENV, "1")
            .env("ORT_DYLIB_PATH", "/nonexistent/libonnxruntime.so")
            .status()
            .unwrap();

        assert!(status.success(), "child test failed: {status}");
    }

    #[cfg(ort_load_dynamic)]
    #[test]
    #[ignore = "only meaningful in the fresh process the test above spawns"]
    fn missing_runtime_library_child() {
        if std::env::var_os(MISSING_RUNTIME_CHILD_ENV).is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(&dir, "identity.onnx", &identity_model::encode());

        let err = session(&path, 1).unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelLoadFailed);
        assert!(err.to_string().contains("ONNX Runtime library"), "{err}");
    }

    /// Downloads multilingual-e5-small from its pinned registry entries (into
    /// `FYDE_TEST_MODELS_DIR` when set, to reuse them across runs) and embeds
    /// one query with it.
    #[tokio::test]
    #[ignore = "downloads a 135 MB model from Hugging Face"]
    async fn multilingual_e5_small_embeds_a_query() {
        use crate::tools::models::{
            MULTILINGUAL_E5_SMALL, MULTILINGUAL_E5_SMALL_TOKENIZER, ModelStore,
        };

        let temp = tempfile::tempdir().unwrap();
        let dir = std::env::var_os("FYDE_TEST_MODELS_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| temp.path().to_path_buf());
        let store = ModelStore::new(dir).unwrap();
        let model_path = store.ensure(&MULTILINGUAL_E5_SMALL).await.unwrap();
        let tokenizer_path = store
            .ensure(&MULTILINGUAL_E5_SMALL_TOKENIZER)
            .await
            .unwrap();

        let tokenizer = tokenizer(&tokenizer_path).unwrap();
        let encoding = tokenizer
            .encode("query: combien coûte mon assurance habitation ?", true)
            .unwrap();
        let len = encoding.get_ids().len();
        let as_i64 = |values: &[u32]| values.iter().map(|&v| i64::from(v)).collect::<Vec<_>>();

        let mut session = session(&model_path, 1).unwrap();
        let names: Vec<String> = session
            .inputs()
            .iter()
            .map(|input| input.name().to_string())
            .collect();
        let mut inputs = Vec::new();
        for name in names {
            let values = match name.as_str() {
                "input_ids" => as_i64(encoding.get_ids()),
                "attention_mask" => as_i64(encoding.get_attention_mask()),
                "token_type_ids" => as_i64(encoding.get_type_ids()),
                other => panic!("unexpected model input {other}"),
            };
            inputs.push((name, Tensor::from_array(([1usize, len], values)).unwrap()));
        }
        let outputs = session.run(inputs).unwrap();
        let (shape, values) = outputs[0].try_extract_tensor::<f32>().unwrap();

        assert_eq!(&**shape, &[1, len as i64, 384]);
        assert!(values.iter().all(|v| v.is_finite()));
        assert!(values.iter().any(|&v| v != 0.0));
    }
}
