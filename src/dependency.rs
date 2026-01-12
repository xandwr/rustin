//! Dependency Bridge - Maps Cargo.lock to registry sources
//!
//! This module resolves dependencies by:
//! 1. Parsing Cargo.lock to get exact versions
//! 2. Locating source files in ~/.cargo/registry/src/...
//! 3. Extracting only the public API of those crates

use crate::parser::PartialParser;
use crate::types::*;
use cargo_metadata::{MetadataCommand, Package};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DependencyError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("Cargo metadata error: {0}")]
    Metadata(#[from] cargo_metadata::Error),
    #[error("Registry not found: {0}")]
    RegistryNotFound(String),
    #[error("Crate not found: {0}")]
    CrateNotFound(String),
}

/// Cargo.lock structure for parsing
#[derive(Debug, Deserialize)]
struct CargoLock {
    package: Option<Vec<LockPackage>>,
}

#[derive(Debug, Deserialize)]
struct LockPackage {
    name: String,
    version: String,
    source: Option<String>,
}

/// Bridge between your project and its dependencies
pub struct DependencyBridge {
    /// Path to the project root
    project_root: PathBuf,
    /// Path to cargo registry
    registry_path: PathBuf,
    /// Cached dependency information
    dependencies: HashMap<String, CrateDependency>,
    /// Parser for extracting APIs
    parser: PartialParser,
}

impl DependencyBridge {
    /// Create a new dependency bridge for a project
    pub fn new(project_root: &Path) -> Result<Self, DependencyError> {
        let registry_path = Self::find_registry_path()?;

        Ok(Self {
            project_root: project_root.to_path_buf(),
            registry_path,
            dependencies: HashMap::new(),
            parser: PartialParser::new(),
        })
    }

    /// Find the cargo registry path
    fn find_registry_path() -> Result<PathBuf, DependencyError> {
        // Try common locations
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());

        let candidates = [
            PathBuf::from(&home).join(".cargo/registry/src"),
            PathBuf::from(&home).join(".rustup/toolchains"),
        ];

        for candidate in &candidates {
            if candidate.exists() {
                return Ok(candidate.clone());
            }
        }

        // Fall back to home/.cargo/registry/src even if it doesn't exist yet
        Ok(PathBuf::from(&home).join(".cargo/registry/src"))
    }

    /// Load all dependencies from Cargo.lock
    pub fn load_dependencies(
        &mut self,
    ) -> Result<&HashMap<String, CrateDependency>, DependencyError> {
        let lock_path = self.project_root.join("Cargo.lock");

        if !lock_path.exists() {
            return Ok(&self.dependencies);
        }

        let lock_content = std::fs::read_to_string(&lock_path)?;
        let lock: CargoLock = toml::from_str(&lock_content)?;

        if let Some(packages) = lock.package {
            for pkg in packages {
                if pkg.source.is_some() {
                    // External dependency
                    let registry_path = self.find_crate_in_registry(&pkg.name, &pkg.version);

                    self.dependencies.insert(
                        pkg.name.clone(),
                        CrateDependency {
                            name: pkg.name,
                            version: pkg.version,
                            source: pkg.source,
                            registry_path,
                            public_api: Vec::new(),
                        },
                    );
                }
            }
        }

        Ok(&self.dependencies)
    }

    /// Find a crate's source in the registry
    fn find_crate_in_registry(&self, name: &str, version: &str) -> Option<PathBuf> {
        // Registry sources are typically in:
        // ~/.cargo/registry/src/index.crates.io-*/cratename-version/

        if !self.registry_path.exists() {
            return None;
        }

        // Find the index directory (e.g., index.crates.io-6f17d22bba15001f)
        let entries = std::fs::read_dir(&self.registry_path).ok()?;

        for entry in entries.flatten() {
            let index_path = entry.path();
            if index_path.is_dir() {
                let crate_dir = index_path.join(format!("{}-{}", name, version));
                if crate_dir.exists() {
                    return Some(crate_dir);
                }
            }
        }

        None
    }

    /// Get detailed metadata using cargo_metadata
    pub fn get_metadata(&self) -> Result<Vec<Package>, DependencyError> {
        let metadata = MetadataCommand::new()
            .manifest_path(self.project_root.join("Cargo.toml"))
            .exec()?;

        Ok(metadata.packages)
    }

    /// Extract public API from a dependency
    pub fn extract_public_api(
        &mut self,
        crate_name: &str,
    ) -> Result<Vec<ParsedItem>, DependencyError> {
        // Check if we already have the API cached
        if let Some(dep) = self.dependencies.get(crate_name) {
            if !dep.public_api.is_empty() {
                return Ok(dep.public_api.clone());
            }
        }

        // Find the crate source
        let dep = self
            .dependencies
            .get(crate_name)
            .ok_or_else(|| DependencyError::CrateNotFound(crate_name.to_string()))?;

        let registry_path = dep
            .registry_path
            .clone()
            .ok_or_else(|| DependencyError::CrateNotFound(crate_name.to_string()))?;

        // Parse the crate's lib.rs or main entry point
        let lib_rs = registry_path.join("src/lib.rs");
        let entry_point = if lib_rs.exists() {
            lib_rs
        } else {
            registry_path.join("src/main.rs")
        };

        if !entry_point.exists() {
            return Ok(Vec::new());
        }

        // Parse and filter to public items only
        let parsed = self
            .parser
            .parse_file(&entry_point)
            .map_err(|e| DependencyError::Io(std::io::Error::other(e.to_string())))?;

        let public_items: Vec<ParsedItem> = parsed
            .items
            .into_iter()
            .filter(|item| matches!(item.visibility, Visibility::Public))
            .collect();

        // Cache the result
        if let Some(dep) = self.dependencies.get_mut(crate_name) {
            dep.public_api = public_items.clone();
        }

        Ok(public_items)
    }

    /// Resolve a path like `tokio::spawn` to its source location
    pub fn resolve_path(&mut self, path: &str) -> Option<ResolvedPath> {
        let parts: Vec<&str> = path.split("::").collect();
        if parts.is_empty() {
            return None;
        }

        let crate_name = parts[0];

        // Check if it's a known dependency
        if !self.dependencies.contains_key(crate_name) {
            // Try to load it
            let _ = self.load_dependencies();
        }

        // Extract public API if not cached
        {
            let dep = self.dependencies.get(crate_name)?;
            if dep.public_api.is_empty() {
                let _ = dep; // Release borrow before mutable call
                let _ = self.extract_public_api(crate_name);
            }
        }

        let dep = self.dependencies.get(crate_name)?;
        let registry_path = dep.registry_path.clone()?;

        // Search for the item in the public API
        let item_name = parts.last()?;
        let found_item = dep.public_api.iter().find(|item| item.name == *item_name)?;

        Some(ResolvedPath {
            crate_name: crate_name.to_string(),
            item_name: item_name.to_string(),
            file_path: found_item.file_path.clone(),
            span: found_item.span,
            kind: found_item.kind.clone(),
            registry_path,
        })
    }

    /// Get all dependencies
    pub fn get_dependencies(&self) -> &HashMap<String, CrateDependency> {
        &self.dependencies
    }

    /// Recursively extract all public items from a crate (including submodules)
    pub fn extract_full_public_api(
        &self,
        crate_name: &str,
    ) -> Result<Vec<ParsedItem>, DependencyError> {
        let dep = self
            .dependencies
            .get(crate_name)
            .ok_or_else(|| DependencyError::CrateNotFound(crate_name.to_string()))?;

        let registry_path = dep
            .registry_path
            .clone()
            .ok_or_else(|| DependencyError::CrateNotFound(crate_name.to_string()))?;

        let src_path = registry_path.join("src");
        if !src_path.exists() {
            return Ok(Vec::new());
        }

        // Parse entire src directory
        let parsed_files = self
            .parser
            .parse_project(&src_path)
            .map_err(|e| DependencyError::Io(std::io::Error::other(e.to_string())))?;

        // Collect all public items
        let public_items: Vec<ParsedItem> = parsed_files
            .into_iter()
            .flat_map(|f| f.items)
            .filter(|item| matches!(item.visibility, Visibility::Public))
            .collect();

        Ok(public_items)
    }
}

/// Result of resolving a path to its source
#[derive(Debug, Clone)]
pub struct ResolvedPath {
    pub crate_name: String,
    pub item_name: String,
    pub file_path: PathBuf,
    pub span: Span,
    pub kind: ItemKind,
    pub registry_path: PathBuf,
}

impl std::fmt::Display for ResolvedPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}::{} at {}:{}",
            self.crate_name,
            self.item_name,
            self.file_path.display(),
            self.span.start_line
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_registry() {
        let registry = DependencyBridge::find_registry_path();
        assert!(registry.is_ok());
    }
}
