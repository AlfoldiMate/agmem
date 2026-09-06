//! Where ONNX Runtime runs the model (issue #139).
//!
//! One graph, one runtime: an accelerator is an ONNX Runtime *execution
//! provider* registered on the session, never a second inference engine.
//! The CPU provider is always there. The CoreML provider exists in every
//! macOS build of the runtime agmem links, but its Rust surface is behind the
//! `coreml` cargo feature, so a build without it can only ever resolve to
//! [`Active::Cpu`].
//!
//! `metal` is the one exception to "one runtime" (issue #178): it names
//! llama.cpp's Metal backend, exists only behind the `llama` feature, is a
//! measurement surface like `candidates`, and never resolves from `auto`.
//!
//! `auto` is resolved once, before any model loads, so the daemon, `doctor`
//! and a spawned child all hold — and print — the same concrete answer.

#[cfg(any(feature = "coreml", feature = "candidates"))]
use std::path::Path;

use crate::EmbedError;

/// What the configuration asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Accelerator {
    /// CoreML where the build and the machine both have it, else CPU.
    #[default]
    Auto,
    /// The CPU provider only — the portable default.
    Cpu,
    /// The CoreML provider; an error when the build or the machine lacks it.
    CoreMl,
    /// llama.cpp's Metal backend (#178); an error without the `llama`
    /// feature, and only a GGUF candidate can run on it.
    Metal,
}

impl Accelerator {
    /// The spelling `--accelerator` takes.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cpu => "cpu",
            Self::CoreMl => "coreml",
            Self::Metal => "metal",
        }
    }

    /// The accelerator a spelling names.
    #[must_use]
    pub fn parse(spelling: &str) -> Option<Self> {
        match spelling {
            "auto" => Some(Self::Auto),
            "cpu" => Some(Self::Cpu),
            "coreml" => Some(Self::CoreMl),
            "metal" => Some(Self::Metal),
            _ => None,
        }
    }

    /// Settle `auto` against this build and this machine.
    ///
    /// # Errors
    /// [`EmbedError::Backend`] when CoreML was asked for by name and this
    /// build was made without the `coreml` feature, or the runtime reports
    /// the provider unavailable here.
    pub fn resolve(self) -> Result<Active, EmbedError> {
        match self {
            Self::Cpu => Ok(Active::Cpu),
            Self::Auto => Ok(if coreml_available() {
                Active::CoreMl
            } else {
                Active::Cpu
            }),
            Self::CoreMl if coreml_available() => Ok(Active::CoreMl),
            Self::CoreMl => Err(EmbedError::Backend {
                backend: "accelerator",
                message: if cfg!(feature = "coreml") {
                    "the CoreML execution provider is not available on this machine".to_owned()
                } else {
                    "this build has no CoreML support; build with `--features coreml` on macOS"
                        .to_owned()
                },
            }),
            Self::Metal if cfg!(all(feature = "llama-metal", target_os = "macos")) => {
                Ok(Active::Metal)
            }
            Self::Metal => Err(EmbedError::Backend {
                backend: "accelerator",
                message: "this build has no Metal support; build with `--features llama-metal` \
                          on macOS"
                    .to_owned(),
            }),
        }
    }
}

/// What the session actually runs on, once `auto` is settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Active {
    /// The CPU provider only.
    Cpu,
    /// The CoreML provider, with the CPU provider behind it for the ops
    /// Core ML has no kernel for (embedding lookups, the mask path).
    CoreMl,
    /// llama.cpp's Metal backend, every layer offloaded (#178). Not an
    /// ONNX Runtime provider: an ORT session asked to run here is an error.
    Metal,
}

impl Active {
    /// What `doctor`, the startup log and a latency row print.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::CoreMl => "coreml",
            Self::Metal => "metal",
        }
    }

    /// The providers to register on a session, in priority order.
    ///
    /// Empty for the CPU: ONNX Runtime registers its CPU provider itself.
    /// For CoreML: the `MLProgram` format (the only one still developed),
    /// every compute unit so Core ML may place fp16-safe ops on the Neural
    /// Engine, and a compiled-model cache under `cache_dir` so the
    /// seconds-long first compile happens once per model, not once per
    /// process. Registration failure is an error, not a silent fall back
    /// to the CPU: a row measured under `coreml` must have run on it.
    #[cfg(any(feature = "coreml", feature = "candidates"))]
    #[must_use]
    pub fn execution_providers(
        self,
        cache_dir: Option<&Path>,
    ) -> Vec<ort::ep::ExecutionProviderDispatch> {
        match self {
            Self::Cpu => {
                let _ = cache_dir; // only the CoreML arm reads it
                Vec::new()
            }
            #[cfg(feature = "coreml")]
            Self::CoreMl => {
                use ort::ep::{CoreML, coreml::ComputeUnits, coreml::ModelFormat};
                let mut ep = CoreML::default()
                    .with_model_format(ModelFormat::MLProgram)
                    .with_compute_units(ComputeUnits::All);
                if let Some(dir) = cache_dir {
                    let dir = dir.join("coreml");
                    if std::fs::create_dir_all(&dir).is_ok() {
                        ep = ep.with_model_cache_dir(dir.display());
                    }
                }
                vec![ep.build().error_on_failure()]
            }
            #[cfg(not(feature = "coreml"))]
            Self::CoreMl => unreachable!("CoreMl never resolves without the coreml feature"),
            Self::Metal => unreachable!("Metal is llama.cpp's; no ORT session is built for it"),
        }
    }
}

/// Whether the runtime this binary links was compiled with the CoreML
/// provider *and* the Rust surface for it is in this build.
#[cfg(feature = "coreml")]
fn coreml_available() -> bool {
    use ort::ep::ExecutionProvider as _;
    ort::ep::CoreML::default().is_available().unwrap_or(false)
}

#[cfg(not(feature = "coreml"))]
fn coreml_available() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spellings_round_trip() {
        for accelerator in [
            Accelerator::Auto,
            Accelerator::Cpu,
            Accelerator::CoreMl,
            Accelerator::Metal,
        ] {
            assert_eq!(Accelerator::parse(accelerator.as_str()), Some(accelerator));
        }
        assert_eq!(Accelerator::parse("gpu"), None);
    }

    #[test]
    fn cpu_always_resolves_to_cpu() {
        assert_eq!(Accelerator::Cpu.resolve().unwrap(), Active::Cpu);
    }

    #[test]
    fn auto_never_fails() {
        // Whichever it lands on, `auto` is the one spelling that must load
        // on every machine this binary ships to.
        let _ = Accelerator::Auto.resolve().expect("auto resolves");
    }

    #[cfg(not(feature = "coreml"))]
    #[test]
    fn coreml_by_name_is_refused_without_the_feature() {
        assert_eq!(Accelerator::Auto.resolve().unwrap(), Active::Cpu);
        let err = Accelerator::CoreMl.resolve().unwrap_err();
        assert!(err.to_string().contains("--features coreml"), "{err}");
    }
}
