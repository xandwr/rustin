//! Rustin - Rust Architecture Analysis Tool
//!
//! A CLI tool for analyzing Rust project architecture with:
//! - Resilient partial parsing (handles broken code)
//! - Dependency bridge to cargo registry sources
//! - Semantic gravity ranking for intelligent search
//! - Call-site teleportation (local usage of external symbols)

use rustin::{DependencyBridge, SemanticGravity};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let project_root = if args.len() > 1 {
        PathBuf::from(&args[1])
    } else {
        std::env::current_dir().expect("Failed to get current directory")
    };

    println!("Rustin - Rust Architecture Analyzer");
    println!("====================================\n");
    println!("Analyzing: {}\n", project_root.display());

    // Initialize components
    let mut gravity = SemanticGravity::new();
    let mut dep_bridge = match DependencyBridge::new(&project_root) {
        Ok(bridge) => Some(bridge),
        Err(e) => {
            eprintln!("Warning: Could not initialize dependency bridge: {}", e);
            None
        }
    };

    // Analyze the project
    println!("Phase 1: Parsing project files...");
    if let Err(e) = gravity.analyze_project(&project_root) {
        eprintln!("Error analyzing project: {}", e);
        return;
    }

    let files = gravity.get_files();
    let total_errors: usize = files.iter().map(|f| f.parse_errors.len()).sum();
    println!(
        "  Parsed {} files ({} with partial recovery)",
        files.len(),
        total_errors
    );

    // Load dependencies
    println!("\nPhase 2: Resolving dependencies...");
    if let Some(ref mut bridge) = dep_bridge {
        match bridge.load_dependencies() {
            Ok(deps) => {
                println!("  Found {} external dependencies", deps.len());
                for (name, dep) in deps.iter().take(10) {
                    let status = if dep.registry_path.is_some() {
                        "+"
                    } else {
                        "?"
                    };
                    println!("    {} {} v{}", status, name, dep.version);
                }
                if deps.len() > 10 {
                    println!("    ... and {} more", deps.len() - 10);
                }
            }
            Err(e) => {
                eprintln!("  Warning: Could not load dependencies: {}", e);
            }
        }
    }

    // Generate summary
    println!("\nPhase 3: Computing semantic gravity...");
    let summary = gravity.summarize();
    println!("\n{}", summary);

    // Show top external symbols used
    let external_symbols = gravity.get_all_external_symbols();
    if !external_symbols.is_empty() {
        println!("=== External Symbol Usage ===");
        for (path, count) in external_symbols.iter().take(10) {
            println!("  {} ({} usages)", path, count);
        }
        if external_symbols.len() > 10 {
            println!("  ... and {} more", external_symbols.len() - 10);
        }
    }

    // Demo: search functionality
    if args.len() > 2 {
        let query = &args[2];
        println!("\n=== Search Results for '{}' ===", query);

        let results = gravity.search(query);
        for (i, result) in results.iter().take(10).enumerate() {
            let test_marker = if result.factors.is_test {
                " [TEST]"
            } else {
                ""
            };
            println!(
                "{}. {}{} (score: {:.1})",
                i + 1,
                result.item.name,
                test_marker,
                result.score
            );
            println!(
                "   File: {}:{}",
                result.item.file_path.display(),
                result.item.span.start_line
            );
            println!(
                "   Factors: x-mod={}, generics={}, calls={}, site={}",
                result.factors.cross_module_count,
                result.factors.generic_depth,
                result.factors.call_count,
                result.factors.is_site
            );

            if result.factors.impl_count > 0 {
                println!(
                    "   Impls: {} ({:?})",
                    result.factors.impl_count, result.factors.trait_impls
                );
            }
            println!();
        }
    }

    // Demo: resolve external path with local usage bridge
    if args.len() > 3 && args[3].contains("::") {
        let path = &args[3];
        println!("\n=== Call-Site Teleportation for '{}' ===", path);

        // First show local usages (the "bridge")
        let local_usages = gravity.get_external_usages(path);
        if !local_usages.is_empty() {
            println!(
                "\nLocal usages in your project ({} sites):",
                local_usages.len()
            );

            // Sort by complexity
            let mut sorted_usages: Vec<_> = local_usages.iter().collect();
            sorted_usages.sort_by(|a, b| b.complexity.cmp(&a.complexity));

            for (i, usage) in sorted_usages.iter().take(5).enumerate() {
                let complexity_label = match usage.complexity {
                    0..=2 => "simple",
                    3..=5 => "moderate",
                    _ => "complex",
                };
                println!(
                    "  {}. {}:{} in {}() [{}]",
                    i + 1,
                    usage.file.display(),
                    usage.line,
                    usage.caller_context,
                    complexity_label
                );
            }

            if let Some(most_complex) = gravity.get_most_complex_usage(path) {
                println!(
                    "\n  Most complex usage: {}:{} in {}()",
                    most_complex.file.display(),
                    most_complex.line,
                    most_complex.caller_context
                );
            }
        } else {
            println!("  No local usages found for '{}'", path);
        }

        // Then show registry location
        if let Some(ref mut bridge) = dep_bridge {
            match bridge.resolve_path(path) {
                Some(resolved) => {
                    println!("\nRegistry source:");
                    println!("  {}", resolved);
                    println!("  Path: {}", resolved.registry_path.display());
                }
                None => {
                    println!("\n  Could not resolve in registry");
                }
            }
        }
    }

    println!("\n=== Analysis Complete ===");
    println!("\nUsage:");
    println!("  rustin [project_path] [search_query] [crate::path::to::resolve]");
    println!("\nExamples:");
    println!("  rustin .                    # Analyze current directory");
    println!("  rustin . parse              # Search for 'parse'");
    println!("  rustin . spawn tokio::spawn # Search and show local usage bridge");
}

/// Interactive analysis session (for future REPL mode)
#[allow(dead_code)]
struct AnalysisSession {
    gravity: SemanticGravity,
    dep_bridge: Option<DependencyBridge>,
    project_root: PathBuf,
}

#[allow(dead_code)]
impl AnalysisSession {
    fn new(project_root: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        let mut gravity = SemanticGravity::new();
        gravity.analyze_project(&project_root)?;

        let dep_bridge = DependencyBridge::new(&project_root).ok();

        Ok(Self {
            gravity,
            dep_bridge,
            project_root,
        })
    }

    /// Search for items by name
    fn search(&self, query: &str) -> Vec<rustin::WorkSiteScore> {
        self.gravity.search(query)
    }

    /// Get all implementations for a type
    fn get_impls(&self, type_name: &str) -> Vec<&rustin::ParsedItem> {
        self.gravity.get_impls_for_type(type_name)
    }

    /// Find where a function is called
    fn find_callers(&self, fn_name: &str) -> Vec<&rustin::CallSite> {
        self.gravity.find_call_sites(fn_name)
    }

    /// Resolve an external crate path
    fn resolve_external(&mut self, path: &str) -> Option<rustin::dependency::ResolvedPath> {
        self.dep_bridge.as_mut()?.resolve_path(path)
    }

    /// Get local usages of an external symbol
    fn get_local_usages(&self, path: &str) -> Vec<&rustin::ExternalReference> {
        self.gravity.get_external_usages(path)
    }

    /// Get the project summary
    fn summary(&self) -> rustin::gravity::ProjectSummary {
        self.gravity.summarize()
    }
}
