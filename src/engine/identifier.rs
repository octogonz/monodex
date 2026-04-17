//! Identifier validation and composition for monodex.
//!
//! This module centralizes all identifier handling to ensure consistency
//! across the codebase. See the syntax spec at:
//! https://github.com/microsoft/monodex/issues/25
//!
//! ## Terminology
//!
//! - **catalog**: A bare identifier naming a catalog (e.g., "my-repo")
//! - **label**: A bare identifier naming a label (e.g., "main", "feature/x")
//! - **label_id**: The qualified storage form "catalog:label" (e.g., "my-repo:main")
//!
//! The user-facing API always takes separate `--catalog` and `--label` flags.
//! The qualified `label_id` form is internal only and never shown to users.

use std::fmt;
use std::str::FromStr;
use thiserror::Error;

// ============================================================================
// Error Types
// ============================================================================

/// Error codes for identifier validation failures.
#[derive(Debug, Clone, Error)]
pub enum IdentifierError {
    /// Catalog name is invalid.
    #[error("[invalid_catalog] {message}")]
    Catalog { code: &'static str, message: String },

    /// Label name is invalid.
    #[error("[invalid_label] {message}")]
    Label { code: &'static str, message: String },

    /// Label ID has invalid syntax.
    #[error("[invalid_label_id] {message}")]
    LabelId { code: &'static str, message: String },

    /// Relative path is invalid.
    #[error("[invalid_path] {message}")]
    Path { code: &'static str, message: String },
}

impl IdentifierError {
    /// Returns the error code for programmatic handling.
    #[allow(dead_code)]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Catalog { code, .. } => code,
            Self::Label { code, .. } => code,
            Self::LabelId { code, .. } => code,
            Self::Path { code, .. } => code,
        }
    }
}

// ============================================================================
// Validation Constants
// ============================================================================

/// Maximum length for catalog and label names.
const MAX_IDENTIFIER_LENGTH: usize = 128;

// ============================================================================
// Validation Functions
// ============================================================================

/// Validates a catalog name (kebab-case).
///
/// Rule: `^[a-z0-9]+(?:-[a-z0-9]+)*$`
///
/// Examples: `my-repo`, `frontend`, `backend-api`
pub fn validate_catalog(name: &str) -> Result<(), IdentifierError> {
    if name.is_empty() {
        return Err(IdentifierError::Catalog {
            code: "catalog_empty",
            message: "Catalog name cannot be empty".to_string(),
        });
    }

    if name.len() > MAX_IDENTIFIER_LENGTH {
        return Err(IdentifierError::Catalog {
            code: "catalog_too_long",
            message: format!(
                "Catalog name exceeds maximum length of {} characters",
                MAX_IDENTIFIER_LENGTH
            ),
        });
    }

    // Kebab-case: lowercase alphanumeric with hyphens
    let valid = name.split('-').all(|part| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    });

    if !valid {
        return Err(IdentifierError::Catalog {
            code: "catalog_invalid_format",
            message: "Catalog name must be kebab-case (lowercase alphanumeric with hyphens, e.g., 'my-repo')".to_string(),
        });
    }

    Ok(())
}

/// Validates a label name (Git-like identifier).
///
/// Rule: `^[a-z0-9]+(?:[./-][a-z0-9]+)*$`
///
/// Also accepts typed form `kind=payload` where:
/// - kind: `^[a-z0-9]+$`
/// - payload: follows label rules above
///
/// Examples: `main`, `feature/x`, `release/v1.2.3`, `branch=main`, `commit=abc123`
pub fn validate_label(name: &str) -> Result<(), IdentifierError> {
    if name.is_empty() {
        return Err(IdentifierError::Label {
            code: "label_empty",
            message: "Label name cannot be empty".to_string(),
        });
    }

    if name.len() > MAX_IDENTIFIER_LENGTH {
        return Err(IdentifierError::Label {
            code: "label_too_long",
            message: format!(
                "Label name exceeds maximum length of {} characters",
                MAX_IDENTIFIER_LENGTH
            ),
        });
    }

    // Check for typed form: kind=payload
    if let Some(eq_idx) = name.find('=') {
        let kind = &name[..eq_idx];
        let payload = &name[eq_idx + 1..];

        // Validate kind: alphanumeric only
        if kind.is_empty()
            || !kind
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            return Err(IdentifierError::Label {
                code: "label_invalid_kind",
                message: format!(
                    "Typed label kind must be alphanumeric (e.g., 'branch', 'commit'), got '{}'",
                    kind
                ),
            });
        }

        // Validate payload using the label payload rules
        validate_label_payload(payload)?;
        return Ok(());
    }

    // Non-typed form: validate as label payload
    validate_label_payload(name)
}

/// Validates a label payload (the part after `=` in typed forms, or the whole label).
///
/// Rule: `^[a-z0-9]+(?:[./-][a-z0-9]+)*$`
fn validate_label_payload(payload: &str) -> Result<(), IdentifierError> {
    if payload.is_empty() {
        return Err(IdentifierError::Label {
            code: "label_payload_empty",
            message: "Label payload cannot be empty".to_string(),
        });
    }

    // Split on separators and validate each segment
    let mut segment_len = 0usize;

    for c in payload.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            segment_len += 1;
        } else if c == '.' || c == '/' || c == '-' {
            // Separator: must have at least one char before it
            if segment_len == 0 {
                return Err(IdentifierError::Label {
                    code: "label_payload_invalid_format",
                    message: format!(
                        "Label payload '{}' has invalid format: segments must be alphanumeric separated by '.', '/', or '-'",
                        payload
                    ),
                });
            }
            segment_len = 0;
        } else {
            return Err(IdentifierError::Label {
                code: "label_payload_invalid_char",
                message: format!(
                    "Label payload '{}' contains invalid character '{}'. Allowed: lowercase letters, digits, '.', '/', '-'",
                    payload, c
                ),
            });
        }
    }

    // Must end with an alphanumeric segment
    if segment_len == 0 {
        return Err(IdentifierError::Label {
            code: "label_payload_trailing_separator",
            message: format!("Label payload '{}' cannot end with a separator", payload),
        });
    }

    Ok(())
}

/// Validates a relative path for crawl-time indexing.
///
/// Paths cannot contain reserved grammar characters: `:`, `@`, or `=`.
/// These characters are reserved for future reference syntax.
pub fn validate_relative_path(path: &str) -> Result<(), IdentifierError> {
    if path.is_empty() {
        return Err(IdentifierError::Path {
            code: "path_empty",
            message: "Relative path cannot be empty".to_string(),
        });
    }

    for c in path.chars() {
        if c == ':' {
            return Err(IdentifierError::Path {
                code: "path_contains_colon",
                message: format!(
                    "Relative path '{}' contains ':', which is reserved for future reference syntax",
                    path
                ),
            });
        }
        if c == '@' {
            return Err(IdentifierError::Path {
                code: "path_contains_at",
                message: format!(
                    "Relative path '{}' contains '@', which is reserved for future reference syntax",
                    path
                ),
            });
        }
        if c == '=' {
            return Err(IdentifierError::Path {
                code: "path_contains_equals",
                message: format!(
                    "Relative path '{}' contains '=', which is reserved for future reference syntax",
                    path
                ),
            });
        }
    }

    Ok(())
}

// ============================================================================
// LabelId Newtype
// ============================================================================

/// A validated label identifier in storage form (catalog:label).
///
/// This type guarantees that the label_id has been validated and contains
/// only allowed characters. It is used internally for Qdrant storage and
/// is never shown directly to users.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LabelId {
    catalog: String,
    label: String,
    combined: String,
}

impl LabelId {
    /// Creates a new LabelId from validated catalog and label names.
    pub fn new(catalog: &str, label: &str) -> Result<Self, IdentifierError> {
        validate_catalog(catalog)?;
        validate_label(label)?;

        Ok(Self {
            catalog: catalog.to_string(),
            label: label.to_string(),
            combined: format!("{}:{}", catalog, label),
        })
    }

    /// Creates a LabelId without validation (for reading from Qdrant).
    #[allow(dead_code)]
    pub fn new_unchecked(catalog: &str, label: &str) -> Self {
        Self {
            catalog: catalog.to_string(),
            label: label.to_string(),
            combined: format!("{}:{}", catalog, label),
        }
    }

    /// Parses a label_id string (catalog:label format).
    pub fn parse(s: &str) -> Result<Self, IdentifierError> {
        let parts: Vec<&str> = s.splitn(2, ':').collect();
        if parts.len() != 2 {
            return Err(IdentifierError::LabelId {
                code: "label_id_missing_colon",
                message: format!("Label ID '{}' must be in 'catalog:label' format", s),
            });
        }
        Self::new(parts[0], parts[1])
    }

    /// Returns the catalog component.
    #[allow(dead_code)]
    pub fn catalog(&self) -> &str {
        &self.catalog
    }

    /// Returns the label component.
    #[allow(dead_code)]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the full label_id string.
    pub fn as_str(&self) -> &str {
        &self.combined
    }
}

impl fmt::Display for LabelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.combined)
    }
}

impl AsRef<str> for LabelId {
    fn as_ref(&self) -> &str {
        &self.combined
    }
}

impl std::ops::Deref for LabelId {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        &self.combined
    }
}

impl FromStr for LabelId {
    type Err = IdentifierError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl serde::Serialize for LabelId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.combined)
    }
}

impl<'de> serde::Deserialize<'de> for LabelId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Computes the label_id string from catalog and label names.
#[allow(dead_code)]
pub fn compute_label_id(catalog: &str, label: &str) -> Result<String, IdentifierError> {
    validate_catalog(catalog)?;
    validate_label(label)?;
    Ok(format!("{}:{}", catalog, label))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_catalog_valid() {
        assert!(validate_catalog("sparo").is_ok());
        assert!(validate_catalog("rushstack").is_ok());
        assert!(validate_catalog("my-repo").is_ok());
        assert!(validate_catalog("backend-api").is_ok());
    }

    #[test]
    fn test_validate_catalog_invalid() {
        // Empty
        assert_eq!(validate_catalog("").unwrap_err().code(), "catalog_empty");

        // Uppercase not allowed
        assert_eq!(
            validate_catalog("MyRepo").unwrap_err().code(),
            "catalog_invalid_format"
        );

        // Underscore not allowed
        assert_eq!(
            validate_catalog("my_repo").unwrap_err().code(),
            "catalog_invalid_format"
        );

        // Double hyphen
        assert_eq!(
            validate_catalog("my--repo").unwrap_err().code(),
            "catalog_invalid_format"
        );

        // Leading hyphen
        assert_eq!(
            validate_catalog("-repo").unwrap_err().code(),
            "catalog_invalid_format"
        );

        // Trailing hyphen
        assert_eq!(
            validate_catalog("repo-").unwrap_err().code(),
            "catalog_invalid_format"
        );
    }

    #[test]
    fn test_validate_label_valid() {
        assert!(validate_label("main").is_ok());
        assert!(validate_label("feature/x").is_ok());
        assert!(validate_label("release/v1.2.3").is_ok());
        assert!(validate_label("working-dir").is_ok());
        assert!(validate_label("repo/sub/feature").is_ok());
    }

    #[test]
    fn test_validate_label_typed_form() {
        assert!(validate_label("branch=main").is_ok());
        assert!(validate_label("branch=feature/x").is_ok());
        assert!(validate_label("commit=abc123").is_ok());
        assert!(validate_label("tag=v1.2.3").is_ok());
        assert!(validate_label("local=working-dir").is_ok());
    }

    #[test]
    fn test_validate_label_invalid() {
        // Empty
        assert_eq!(validate_label("").unwrap_err().code(), "label_empty");

        // Uppercase
        assert_eq!(
            validate_label("Main").unwrap_err().code(),
            "label_payload_invalid_char"
        );

        // Underscore (invalid in payload)
        assert_eq!(
            validate_label("feature_x").unwrap_err().code(),
            "label_payload_invalid_char"
        );

        // Trailing separator
        assert_eq!(
            validate_label("feature/").unwrap_err().code(),
            "label_payload_trailing_separator"
        );

        // Empty kind
        assert_eq!(
            validate_label("=main").unwrap_err().code(),
            "label_invalid_kind"
        );
    }

    #[test]
    fn test_label_id() {
        let id = LabelId::new("sparo", "main").unwrap();
        assert_eq!(id.catalog(), "sparo");
        assert_eq!(id.label(), "main");
        assert_eq!(id.as_str(), "sparo:main");
    }

    #[test]
    fn test_label_id_parse() {
        let id = LabelId::parse("sparo:main").unwrap();
        assert_eq!(id.catalog(), "sparo");
        assert_eq!(id.label(), "main");

        let err = LabelId::parse("sparo").unwrap_err();
        assert_eq!(err.code(), "label_id_missing_colon");
    }

    #[test]
    fn test_compute_label_id_function() {
        let result = compute_label_id("sparo", "main").unwrap();
        assert_eq!(result, "sparo:main");
    }
}
