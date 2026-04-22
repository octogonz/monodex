//! User configuration loading and types.
//!
//! Purpose: Define config file schema and loading/validation logic.
//! Edit here when: Adding config file fields, changing validation rules,
//! or modifying how config is discovered and loaded.
//! Do not edit here for: CLI flags (see cli.rs), command handlers (see commands/).

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::anyhow;

use crate::engine::system_info::{
    ResolvedEmbeddingConfig, compute_auto_embedding_config, estimate_ram_usage, format_bytes,
    get_physical_core_count,
};
use crate::engine::identifier::validate_catalog;

/// Qdrant configuration
#[derive(Debug, serde::Deserialize)]
pub struct QdrantConfig {
    pub url: Option<String>,
    pub collection: String,
    /// Maximum upload payload size in bytes (default: 30MB)
    /// Qdrant has a 32MB limit; we default to 30MB for safety margin
    #[serde(rename = "maxUploadBytes")]
    pub max_upload_bytes: Option<usize>,
}

impl QdrantConfig {
    /// Default max upload size: 30MB (safely under Qdrant's 32MB limit)
    const DEFAULT_MAX_UPLOAD_BYTES: usize = 30 * 1024 * 1024;

    /// Get the configured max upload bytes, or the default
    pub fn get_max_upload_bytes(&self) -> usize {
        self.max_upload_bytes
            .unwrap_or(Self::DEFAULT_MAX_UPLOAD_BYTES)
    }
}

/// Catalog configuration
#[derive(Debug, serde::Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct CatalogConfig {
    /// Catalog type: currently only "monorepo" is supported
    pub r#type: String,
    /// Path to scan
    pub path: String,
}

impl CatalogConfig {
    /// Supported catalog types
    const SUPPORTED_TYPES: &'static [&'static str] = &["monorepo"];

    /// Validate that the catalog type is supported
    pub fn validate(&self) -> anyhow::Result<()> {
        if !Self::SUPPORTED_TYPES.contains(&self.r#type.as_str()) {
            anyhow::bail!(
                "Unsupported catalog type '{}'. Supported types: {}",
                self.r#type,
                Self::SUPPORTED_TYPES.join(", ")
            );
        }
        Ok(())
    }
}

/// Embedding model configuration
#[derive(Debug, serde::Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingModelConfig {
    /// Number of ONNX model instances (sessions). Primary driver of memory usage.
    /// Allowed values: "auto" or integer >= 1
    #[serde(
        rename = "modelInstances",
        default = "EmbeddingModelConfig::default_model_instances"
    )]
    pub model_instances: EmbeddingSizeValue,

    /// Threads per model instance. CPU tuning only.
    /// Allowed values: "auto" or integer >= 1
    #[serde(
        rename = "threadsPerInstance",
        default = "EmbeddingModelConfig::default_threads_per_instance"
    )]
    pub threads_per_instance: EmbeddingSizeValue,
}

/// A value that can be either "auto" or a specific integer
#[derive(Debug, Clone, PartialEq)]
pub enum EmbeddingSizeValue {
    Auto,
    Exact(usize),
}

impl EmbeddingModelConfig {
    fn default_model_instances() -> EmbeddingSizeValue {
        EmbeddingSizeValue::Auto
    }

    fn default_threads_per_instance() -> EmbeddingSizeValue {
        EmbeddingSizeValue::Auto
    }
}

impl Default for EmbeddingModelConfig {
    fn default() -> Self {
        Self {
            model_instances: Self::default_model_instances(),
            threads_per_instance: Self::default_threads_per_instance(),
        }
    }
}

impl<'de> serde::Deserialize<'de> for EmbeddingSizeValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, Visitor};

        struct EmbeddingSizeValueVisitor;

        impl<'de> Visitor<'de> for EmbeddingSizeValueVisitor {
            type Value = EmbeddingSizeValue;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(r#""auto" or an integer >= 1"#)
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if v == "auto" {
                    Ok(EmbeddingSizeValue::Auto)
                } else {
                    Err(de::Error::custom(r#"expected "auto" or an integer >= 1"#))
                }
            }

            fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if v >= 1 {
                    Ok(EmbeddingSizeValue::Exact(v as usize))
                } else {
                    Err(de::Error::custom("integer must be >= 1"))
                }
            }

            fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if v >= 1 {
                    Ok(EmbeddingSizeValue::Exact(v as usize))
                } else {
                    Err(de::Error::custom("integer must be >= 1"))
                }
            }

            fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                // Accept whole numbers from JSON parsers that serialize integers as floats
                if v >= 1.0 && v.fract() == 0.0 {
                    Ok(EmbeddingSizeValue::Exact(v as usize))
                } else if v < 1.0 {
                    Err(de::Error::custom("integer must be >= 1"))
                } else {
                    Err(de::Error::custom(
                        "expected an integer >= 1, got fractional value",
                    ))
                }
            }
        }

        deserializer.deserialize_any(EmbeddingSizeValueVisitor)
    }
}

/// Main configuration file
#[derive(Debug, serde::Deserialize)]
pub struct Config {
    pub qdrant: QdrantConfig,
    pub catalogs: HashMap<String, CatalogConfig>,
    #[serde(rename = "embeddingModel", default)]
    pub embedding_model: EmbeddingModelConfig,
}

/// Load config from a file path.
/// Validates catalog names and types after parsing.
pub fn load_config(path: &PathBuf) -> anyhow::Result<Config> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("Failed to read config file {}: {}", path.display(), e))?;

    // Parse JSON (for now - will add JSONC support later)
    let config: Config = serde_json::from_str(&content)
        .map_err(|e| anyhow!("Failed to parse config file: {}", e))?;

    // Validate catalog names and types
    for (name, catalog) in &config.catalogs {
        validate_catalog(name)
            .map_err(|e| anyhow!("Invalid catalog name '{}' in config: {}", name, e))?;
        catalog
            .validate()
            .map_err(|e| anyhow!("Invalid catalog '{}': {}", name, e))?;
    }

    Ok(config)
}

// ============================================================================
// B.2: Embedding configuration resolution
// ============================================================================

/// Resolve embedding configuration from config file, applying "auto" heuristic if needed.
/// Returns ResolvedEmbeddingConfig with all resolved values including memory info for warnings.
pub fn resolve_embedding_config(config: &EmbeddingModelConfig) -> ResolvedEmbeddingConfig {
    match (&config.model_instances, &config.threads_per_instance) {
        (EmbeddingSizeValue::Auto, EmbeddingSizeValue::Auto) => {
            // Both auto: compute from system properties
            match compute_auto_embedding_config() {
                Ok(resolved) => {
                    println!(
                        "Auto-detected embedding config: {} instances × {} threads",
                        resolved.model_instances, resolved.threads_per_instance
                    );
                    if resolved.cgroup_limited {
                        println!(
                            "  (Cgroup memory limit detected: {})",
                            format_bytes(resolved.total_ram)
                        );
                    }
                    resolved
                }
                Err(e) => {
                    eprintln!("Warning: Failed to auto-detect embedding config: {}", e);
                    eprintln!("Using fallback: 1 instance × {} threads", num_cpus::get());
                    ResolvedEmbeddingConfig {
                        model_instances: 1,
                        threads_per_instance: num_cpus::get(),
                        total_ram: 0,
                        available_ram: 0,
                        estimated_ram_usage: estimate_ram_usage(1),
                        cgroup_limited: false,
                    }
                }
            }
        }
        (EmbeddingSizeValue::Auto, EmbeddingSizeValue::Exact(threads)) => {
            // Auto instances, explicit threads
            match compute_auto_embedding_config() {
                Ok(resolved) => {
                    println!(
                        "Auto-detected model instances: {} (using explicit {} threads/instance)",
                        resolved.model_instances, threads
                    );
                    ResolvedEmbeddingConfig {
                        threads_per_instance: *threads,
                        ..resolved
                    }
                }
                Err(e) => {
                    eprintln!("Warning: Failed to auto-detect embedding config: {}", e);
                    eprintln!("Using fallback: 1 instance × {} threads", threads);
                    ResolvedEmbeddingConfig {
                        model_instances: 1,
                        threads_per_instance: *threads,
                        total_ram: 0,
                        available_ram: 0,
                        estimated_ram_usage: estimate_ram_usage(1),
                        cgroup_limited: false,
                    }
                }
            }
        }
        (EmbeddingSizeValue::Exact(instances), EmbeddingSizeValue::Auto) => {
            // Explicit instances, auto threads
            let physical_cores = get_physical_core_count();
            let threads = std::cmp::max(1, physical_cores / instances);
            println!(
                "Using explicit {} model instances (auto-detected {} threads/instance)",
                instances, threads
            );
            // Get memory info via compute_auto_embedding_config for cgroup-aware values
            let memory_info = compute_auto_embedding_config()
                .map(|resolved| {
                    (
                        resolved.total_ram,
                        resolved.available_ram,
                        resolved.cgroup_limited,
                    )
                })
                .unwrap_or((0, 0, false));
            ResolvedEmbeddingConfig {
                model_instances: *instances,
                threads_per_instance: threads,
                total_ram: memory_info.0,
                available_ram: memory_info.1,
                estimated_ram_usage: estimate_ram_usage(*instances),
                cgroup_limited: memory_info.2,
            }
        }
        (EmbeddingSizeValue::Exact(instances), EmbeddingSizeValue::Exact(threads)) => {
            // Both explicit
            println!(
                "Using explicit config: {} instances × {} threads/instance",
                instances, threads
            );
            // Get memory info via compute_auto_embedding_config for cgroup-aware values
            let memory_info = compute_auto_embedding_config()
                .map(|resolved| {
                    (
                        resolved.total_ram,
                        resolved.available_ram,
                        resolved.cgroup_limited,
                    )
                })
                .unwrap_or((0, 0, false));
            ResolvedEmbeddingConfig {
                model_instances: *instances,
                threads_per_instance: *threads,
                total_ram: memory_info.0,
                available_ram: memory_info.1,
                estimated_ram_usage: estimate_ram_usage(*instances),
                cgroup_limited: memory_info.2,
            }
        }
    }
}

/// Print memory status and warning if estimated usage exceeds available RAM.
pub fn print_memory_warning(resolved: &ResolvedEmbeddingConfig) {
    // Skip warning if we couldn't get memory info
    if resolved.available_ram == 0 {
        eprintln!("Warning: Could not get memory info for warning check");
        return;
    }

    println!(
        "Currently available system RAM: {}",
        format_bytes(resolved.available_ram)
    );
    println!(
        "Estimated embedding RAM usage: {} ({} instance{})",
        format_bytes(resolved.estimated_ram_usage),
        resolved.model_instances,
        if resolved.model_instances > 1 {
            "s"
        } else {
            ""
        }
    );

    if resolved.estimated_ram_usage > resolved.available_ram {
        let excess_pct =
            ((resolved.estimated_ram_usage as f64 / resolved.available_ram as f64) - 1.0) * 100.0;
        eprintln!();
        eprintln!(
            "🚨 Warning: estimate exceeds available RAM by {:.0}%.",
            excess_pct
        );
        eprintln!("   Consider adjusting \"embeddingModel.modelInstances\" or");
        eprintln!("   \"embeddingModel.threadsPerInstance\" in config.json");
        eprintln!("   Suggestion: start with modelInstances = 1");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_catalog_config_validates_monorepo_type() {
        let config = CatalogConfig {
            r#type: "monorepo".to_string(),
            path: "/some/path".to_string(),
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_catalog_config_rejects_unsupported_type() {
        let config = CatalogConfig {
            r#type: "folder".to_string(),
            path: "/some/path".to_string(),
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("Unsupported catalog type 'folder'")
        );
        assert!(err.to_string().contains("Supported types: monorepo"));
    }

    #[test]
    fn test_catalog_config_rejects_unknown_type() {
        let config = CatalogConfig {
            r#type: "unknown".to_string(),
            path: "/some/path".to_string(),
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("Unsupported catalog type 'unknown'")
        );
    }

    #[test]
    fn test_load_config_validates_catalog_types() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let mut file = std::fs::File::create(&config_path).unwrap();

        // Config with invalid catalog type
        writeln!(
            file,
            r#"{{
                "qdrant": {{ "collection": "test" }},
                "catalogs": {{
                    "test": {{
                        "type": "invalid",
                        "path": "/tmp"
                    }}
                }}
            }}"#
        )
        .unwrap();

        let result = load_config(&config_path);
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Invalid catalog 'test'"));
        assert!(
            err.to_string()
                .contains("Unsupported catalog type 'invalid'")
        );
    }

    #[test]
    fn test_load_config_accepts_monorepo_type() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let mut file = std::fs::File::create(&config_path).unwrap();

        writeln!(
            file,
            r#"{{
                "qdrant": {{ "collection": "test" }},
                "catalogs": {{
                    "sparo": {{
                        "type": "monorepo",
                        "path": "/tmp/sparo"
                    }}
                }}
            }}"#
        )
        .unwrap();

        let config = load_config(&config_path).unwrap();
        assert_eq!(config.catalogs.get("sparo").unwrap().r#type, "monorepo");
    }

    #[test]
    fn test_load_config_accepts_max_upload_bytes() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let mut file = std::fs::File::create(&config_path).unwrap();

        writeln!(
            file,
            r#"{{
                "qdrant": {{ "collection": "test", "maxUploadBytes": 20971520 }},
                "catalogs": {{
                    "sparo": {{
                        "type": "monorepo",
                        "path": "/tmp/sparo"
                    }}
                }}
            }}"#
        )
        .unwrap();

        let config = load_config(&config_path).unwrap();
        assert_eq!(config.qdrant.max_upload_bytes, Some(20971520));
        assert_eq!(config.qdrant.get_max_upload_bytes(), 20971520);
    }

    #[test]
    fn test_load_config_max_upload_bytes_defaults() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let mut file = std::fs::File::create(&config_path).unwrap();

        writeln!(
            file,
            r#"{{
                "qdrant": {{ "collection": "test" }},
                "catalogs": {{
                    "sparo": {{
                        "type": "monorepo",
                        "path": "/tmp/sparo"
                    }}
                }}
            }}"#
        )
        .unwrap();

        let config = load_config(&config_path).unwrap();
        assert_eq!(config.qdrant.max_upload_bytes, None);
        assert_eq!(
            config.qdrant.get_max_upload_bytes(),
            30 * 1024 * 1024 // 30MB default
        );
    }
}
