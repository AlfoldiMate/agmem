//! Domain types. Grows with the schema issues; `SpaceName` lands first
//! because config validation needs it.

use crate::CoreError;

/// A validated space (project scope) name.
///
/// Slug rules: `[a-z0-9-_]`, 1–64 chars. Spaces isolate projects inside one
/// store; the reserved [`SpaceName::user`] space holds cross-project personal
/// memory (`docs/design.md` §2.1).
///
/// ```
/// use agmem_core::SpaceName;
/// let s: SpaceName = "my-project_1".parse()?;
/// assert_eq!(s.as_str(), "my-project_1");
/// assert!("Bad Name!".parse::<SpaceName>().is_err());
/// # Ok::<(), agmem_core::CoreError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct SpaceName(String);

impl SpaceName {
    /// Maximum length of a space name in bytes.
    pub const MAX_LEN: usize = 64;

    /// Create a validated space name.
    ///
    /// # Errors
    /// [`CoreError::InvalidSpaceName`] when the slug rules are violated.
    pub fn new(name: impl Into<String>) -> Result<Self, CoreError> {
        let name = name.into();
        let valid = !name.is_empty()
            && name.len() <= Self::MAX_LEN
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
        if valid {
            Ok(Self(name))
        } else {
            Err(CoreError::InvalidSpaceName(name))
        }
    }

    /// The reserved global space for cross-project personal memory.
    pub fn user() -> Self {
        Self("user".to_owned())
    }

    /// The name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SpaceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for SpaceName {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_slugs() {
        for name in ["default", "user", "a", "my-project_1", &"x".repeat(64)] {
            assert!(SpaceName::new(name).is_ok(), "should accept {name:?}");
        }
    }

    #[test]
    fn rejects_invalid_slugs() {
        for name in [
            "",
            "Upper",
            "has space",
            "dot.dot",
            "../etc",
            "é",
            &"x".repeat(65),
        ] {
            assert!(SpaceName::new(name).is_err(), "should reject {name:?}");
        }
    }
}
