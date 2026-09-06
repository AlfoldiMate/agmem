//! Where llama.cpp runs the model.
//!
//! One runtime, two places to run it: the CPU everywhere, and llama.cpp's
//! Metal backend on Apple Silicon, where every layer is offloaded. Metal is
//! compiled in by the target, not by a feature (`Cargo.toml`), so on an
//! `aarch64-apple-darwin` build it is simply there and `auto` picks it; on
//! every other target `metal` by name is an error and `auto` is the CPU.
//! `--accelerator cpu` stays the opt-out on a Mac.
//!
//! `auto` is resolved once, before any model loads, so the daemon, `doctor`
//! and a spawned child all hold — and print — the same concrete answer.

use crate::EmbedError;

/// Whether this build carries llama.cpp's Metal backend.
const METAL_BUILT: bool = cfg!(all(target_os = "macos", target_arch = "aarch64"));

/// What the configuration asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Accelerator {
    /// Metal where the build has it, else the CPU.
    #[default]
    Auto,
    /// The CPU only — the portable choice, and the opt-out on a Mac.
    Cpu,
    /// llama.cpp's Metal backend; an error on a build without it.
    Metal,
}

impl Accelerator {
    /// The spelling `--accelerator` takes.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cpu => "cpu",
            Self::Metal => "metal",
        }
    }

    /// The accelerator a spelling names.
    #[must_use]
    pub fn parse(spelling: &str) -> Option<Self> {
        match spelling {
            "auto" => Some(Self::Auto),
            "cpu" => Some(Self::Cpu),
            "metal" => Some(Self::Metal),
            _ => None,
        }
    }

    /// Settle `auto` against this build.
    ///
    /// # Errors
    /// [`EmbedError::Backend`] when Metal was asked for by name and this
    /// build was not made for Apple Silicon.
    pub fn resolve(self) -> Result<Active, EmbedError> {
        match self {
            Self::Cpu => Ok(Active::Cpu),
            Self::Auto if METAL_BUILT => Ok(Active::Metal),
            Self::Auto => Ok(Active::Cpu),
            Self::Metal if METAL_BUILT => Ok(Active::Metal),
            Self::Metal => Err(EmbedError::Backend {
                backend: "accelerator",
                message: "this build has no Metal backend; it exists only on Apple Silicon \
                          (aarch64-apple-darwin)"
                    .to_owned(),
            }),
        }
    }
}

/// What the session actually runs on, once `auto` is settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Active {
    /// The CPU only.
    Cpu,
    /// llama.cpp's Metal backend, every layer offloaded.
    Metal,
}

impl Active {
    /// What `doctor`, the startup log and a latency row print.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Metal => "metal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spellings_round_trip() {
        for accelerator in [Accelerator::Auto, Accelerator::Cpu, Accelerator::Metal] {
            assert_eq!(Accelerator::parse(accelerator.as_str()), Some(accelerator));
        }
        assert_eq!(Accelerator::parse("coreml"), None);
    }

    #[test]
    fn cpu_always_resolves_to_cpu() {
        assert_eq!(Accelerator::Cpu.resolve().unwrap(), Active::Cpu);
    }

    #[test]
    fn auto_never_fails_and_agrees_with_metal_by_name() {
        // Whichever it lands on, `auto` is the one spelling that must load
        // on every machine this binary ships to — and where it lands on
        // Metal, asking for Metal by name must agree.
        let auto = Accelerator::Auto.resolve().expect("auto resolves");
        match Accelerator::Metal.resolve() {
            Ok(metal) => assert_eq!(auto, metal),
            Err(err) => {
                assert_eq!(auto, Active::Cpu);
                assert!(err.to_string().contains("Apple Silicon"), "{err}");
            }
        }
    }
}
