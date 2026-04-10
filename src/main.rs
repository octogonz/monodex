//! monodex: Semantic search indexer for Rush monorepos
//!
//! Uses Qdrant vector database with jina-embeddings-v2-base-code embeddings
//! Intelligently chunks code and documentation for high-quality semantic search

mod engine;

use clap::{Parser, Subcommand};
use engine::{
    ParallelEmbedder, SMALL_CHUNK_CHARS,
    chunker::{ChunkContext, chunk_content},
    crawl_config::load_compiled_crawl_config,
    git_ops::{
        build_package_index_for_commit, build_package_index_for_working_dir, enumerate_commit_tree,
        enumerate_working_directory, read_blob_content, read_working_file_content,
        resolve_commit_oid,
    },
    partitioner::{ChunkQualityReport, PartitionConfig, PartitionDebug, partition_typescript},
    uploader::{LabelMetadata, PointResult, QdrantUploader},
    util::compute_label_id,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Qdrant configuration
#[derive(Debug, serde::Deserialize)]
struct QdrantConfig {
    url: Option<String>,
    collection: String,
}

/// Catalog configuration
#[derive(Debug, serde::Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct CatalogConfig {
    /// Catalog type: "monorepo" or "folder"
    r#type: String,
    /// Path to scan
    path: String,
}

/// Main configuration file
#[derive(Debug, serde::Deserialize)]
struct Config {
    qdrant: QdrantConfig,
    catalogs: HashMap<String, CatalogConfig>,
}

/// Rush semantic search crawler for Qdrant
/// https://www.rushstack.io
#[derive(Parser)]
#[command(name = "monodex", version, about)]
struct Cli {
    /// Config file path (default: ~/.config/monodex/config.json)
    #[arg(long)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

/// Available commands
#[derive(Subcommand)]
enum Commands {
    /// Set default catalog and label for subsequent commands
    Use {
        /// Catalog name (optional - shows current context if omitted)
        #[arg(long)]
        catalog: Option<String>,

        /// Label name (optional - shows current context if omitted)
        #[arg(long)]
        label: Option<String>,
    },

    /// Crawl source and index into Qdrant (incremental sync).
    /// Reports warnings when AST chunking fails and fallback is used.
    /// These warnings indicate partitioner defects to investigate.
    Crawl {
        /// Catalog name (from config file)
        #[arg(long)]
        catalog: String,

        /// Label name for this crawl (e.g., "main", "feature-x")
        /// Label ID will be computed as <catalog>:<label>
        #[arg(long)]
        label: String,

        /// Git commit to crawl (defaults to HEAD)
        /// Supports branch names, tags, or commit SHA
        /// Mutually exclusive with --working-dir
        #[arg(long, default_value = "HEAD", conflicts_with = "working_dir")]
        commit: String,

        /// Crawl the working directory instead of a Git commit.
        /// Indexes uncommitted changes. Mutually exclusive with --commit.
        #[arg(long, default_value_t = false, conflicts_with = "commit")]
        working_dir: bool,

        /// Allow files with chunking warnings to participate in incremental skipping
        #[arg(long, default_value_t = false)]
        incremental_warnings: bool,
    },

    /// Purge all chunks from a catalog or entire collection
    Purge {
        /// Catalog name to purge (if not specified, purges entire collection)
        #[arg(long)]
        catalog: Option<String>,

        /// Purge all catalogs (entire collection)
        #[arg(long)]
        all: bool,
    },

    /// Dump chunks for a TypeScript file (for debugging chunking algorithm).
    /// Uses AST-only mode by default to reveal partitioner issues.
    /// Add --with-fallback to see production behavior with fallback mitigation.
    DumpChunks {
        /// TypeScript file path
        #[arg(long)]
        file: PathBuf,

        /// Target chunk size in chars
        #[arg(long, default_value = "6000")]
        target_size: usize,

        /// Show visualization mode (full chunk contents)
        #[arg(long)]
        visualize: bool,

        /// Enable fallback line-based splitting for oversized chunks.
        /// By default, dump-chunks uses strict AST-only mode to reveal
        /// where the partitioner failed to find good split points.
        #[arg(long)]
        with_fallback: bool,

        /// Enable debug logging for partitioning decisions
        #[arg(long)]
        debug: bool,
    },

    /// Search with compact blurb output (for AI assistants)
    Search {
        /// Search query text
        #[arg(long)]
        text: String,

        /// Number of results
        #[arg(long, default_value = "10")]
        limit: usize,

        /// Filter by label (uses default context if not provided)
        /// Format: <catalog>:<label>
        #[arg(long)]
        label: Option<String>,

        /// Filter by catalog (optional - uses label or default context)
        #[arg(long)]
        catalog: Option<String>,
    },

    /// View chunks by their file IDs with optional selectors
    View {
        /// File IDs with optional selectors (can be specified multiple times)
        /// Formats:
        ///   700a4ba232fe9ddc        - all chunks in file
        ///   700a4ba232fe9ddc:3      - chunk 3
        ///   700a4ba232fe9ddc:2-3    - chunks 2 through 3
        ///   700a4ba232fe9ddc:3-end  - chunk 3 through the last chunk
        #[arg(long)]
        id: Vec<String>,

        /// Filter by label (uses default context if not provided)
        /// Format: <catalog>:<label>
        #[arg(long)]
        label: Option<String>,

        /// Show full filesystem paths
        #[arg(long)]
        full_paths: bool,

        /// Omit catalog preamble (show only chunks)
        #[arg(long)]
        chunks_only: bool,
    },

    /// Audit chunking quality across multiple files (AST-only mode).
    /// Scores reflect AST partitioning quality without fallback mitigation.
    /// Use after eliminating crawl warnings to find suboptimal chunk boundaries.
    AuditChunks {
        /// Number of files to sample
        #[arg(long, default_value = "20")]
        count: usize,

        /// Directory to sample from
        #[arg(long)]
        dir: String,
    },
}

const DEFAULT_CONFIG_PATH: &str = "~/.config/monodex/config.json";

/// Context file for storing default catalog/label
const DEFAULT_CONTEXT_PATH: &str = "~/.config/monodex/context.json";

/// Default context for commands
#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
struct DefaultContext {
    /// Default catalog name
    catalog: String,
    /// Default label name
    label: String,
    /// When the context was set
    set_at: String,
}

/// Load default context from file
fn load_default_context() -> Option<DefaultContext> {
    let path = shellexpand::tilde(DEFAULT_CONTEXT_PATH);
    let path = std::path::Path::new(path.as_ref());

    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).ok(),
        Err(_) => None,
    }
}

/// Save default context to file
fn save_default_context(catalog: &str, label: &str) -> anyhow::Result<()> {
    let path = shellexpand::tilde(DEFAULT_CONTEXT_PATH);
    let path = std::path::Path::new(path.as_ref());

    // Create parent directory if needed
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let context = DefaultContext {
        catalog: catalog.to_string(),
        label: label.to_string(),
        set_at: chrono_timestamp(),
    };

    let content = serde_json::to_string_pretty(&context)?;
    std::fs::write(path, content)?;

    Ok(())
}

/// Resolve label ID from explicit flag or default context
/// Returns (label_id, catalog, label_name) or error if neither provided
fn resolve_label_id(
    explicit_label: Option<&str>,
    explicit_catalog: Option<&str>,
) -> anyhow::Result<(String, String, String)> {
    // If explicit label provided, use it
    if let Some(label_str) = explicit_label {
        // Parse label format: <catalog>:<label>
        let parts: Vec<&str> = label_str.splitn(2, ':').collect();
        if parts.len() != 2 {
            return Err(anyhow::anyhow!(
                "Invalid label format '{}'. Expected <catalog>:<label>",
                label_str
            ));
        }
        let catalog = parts[0].to_string();
        let label_name = parts[1].to_string();
        let label_id = compute_label_id(&catalog, &label_name);
        return Ok((label_id, catalog, label_name));
    }

    // Try default context
    if let Some(context) = load_default_context() {
        // If explicit catalog provided, use it with default label
        let catalog = explicit_catalog.unwrap_or(&context.catalog).to_string();
        let label_name = context.label.clone();
        let label_id = compute_label_id(&catalog, &label_name);
        return Ok((label_id, catalog, label_name));
    }

    Err(anyhow::anyhow!(
        "No label specified. Use --label <catalog>:<label> or set a default with 'monodex use <catalog> <label>'"
    ))
}

/// Get current timestamp for logging
fn chrono_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let h = (now / 3600) % 24;
    let m = (now / 60) % 60;
    let s = now % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

/// Format duration in seconds to human-readable string (e.g., "1h 23m" or "5m 30s")
fn format_duration(secs: f64) -> String {
    let total_secs = secs as u64;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let s = total_secs % 60;

    if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else if mins > 0 {
        format!("{}m {}s", mins, s)
    } else {
        format!("{}s", s)
    }
}

/// Format ETA in seconds to human-readable string
fn format_eta(secs: f64) -> String {
    if secs <= 0.0 || !secs.is_finite() {
        return "--".to_string();
    }
    format_duration(secs)
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Load config
    let config_path = cli
        .config
        .unwrap_or_else(|| PathBuf::from(shellexpand::tilde(DEFAULT_CONFIG_PATH).as_ref()));
    let config = load_config(&config_path)?;

    match cli.command {
        Commands::Use { catalog, label } => {
            run_use(catalog.as_deref(), label)?;
        }
        Commands::Crawl {
            catalog,
            label,
            commit,
            working_dir,
            incremental_warnings,
        } => {
            if working_dir {
                run_crawl_working_dir(&config, &catalog, &label, incremental_warnings)?;
            } else {
                run_crawl_label(&config, &catalog, &label, &commit, incremental_warnings)?;
            }
        }
        Commands::Purge { catalog, all } => {
            run_purge(&config, catalog.as_deref(), all)?;
        }
        Commands::DumpChunks {
            file,
            target_size,
            visualize,
            with_fallback,
            debug,
        } => {
            run_dump_chunks(&file, target_size, visualize, with_fallback, debug)?;
        }
        Commands::Search {
            text,
            limit,
            label,
            catalog,
        } => {
            run_search(&config, &text, limit, label.as_deref(), catalog.as_deref())?;
        }
        Commands::View {
            id,
            label,
            full_paths,
            chunks_only,
        } => {
            run_view(&config, &id, label.as_deref(), full_paths, chunks_only)?;
        }
        Commands::AuditChunks { count, dir } => {
            run_audit_chunks(count, dir)?;
        }
    }

    Ok(())
}

fn load_config(path: &PathBuf) -> anyhow::Result<Config> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read config file {}: {}", path.display(), e))?;

    // Parse JSON (for now - will add JSONC support later)
    let config: Config = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("Failed to parse config file: {}", e))?;

    Ok(config)
}

/// Run the `use` command to set default context
fn run_use(catalog: Option<&str>, label: Option<String>) -> anyhow::Result<()> {
    match (catalog, label) {
        (None, None) => {
            // Show current context
            match load_default_context() {
                Some(ctx) => {
                    println!("Current context:");
                    println!("  Catalog: {}", ctx.catalog);
                    println!("  Label: {}", ctx.label);
                    println!("  Label ID: {}:{}", ctx.catalog, ctx.label);
                }
                None => {
                    println!("No default context set.");
                    println!();
                    println!("Usage:");
                    println!("  monodex use --catalog <name> --label <name>");
                }
            }
        }
        (Some(catalog_name), Some(label_name)) => {
            // Set new context
            save_default_context(catalog_name, &label_name)?;

            let label_id = compute_label_id(catalog_name, &label_name);
            println!("✓ Default context set to {}:{}", catalog_name, label_name);
            println!("  Label ID: {}", label_id);
            println!();
            println!("Commands will now use this context when --label is not specified.");
        }
        (Some(_), None) | (None, Some(_)) => {
            // Partial specification - error
            return Err(anyhow::anyhow!(
                "Both --catalog and --label are required to set context.\n\n                Usage:\n  monodex use --catalog <name> --label <name>\n\n                Or run 'monodex use' without arguments to see current context."
            ));
        }
    }

    Ok(())
}

fn run_crawl_label(
    config: &Config,
    catalog_name: &str,
    label_name: &str,
    commit: &str,
    _incremental_warnings: bool,
) -> anyhow::Result<()> {
    use crossbeam_channel::{Receiver, Sender, unbounded};
    use engine::util::{CHUNKER_ID, EMBEDDER_ID, compute_file_id};
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    let total_start = std::time::Instant::now();
    println!("🔍 Starting label-aware crawl...");
    println!("Catalog: {}", catalog_name);
    println!("Label: {}", label_name);

    // Get catalog config
    let catalog_config = config
        .catalogs
        .get(catalog_name)
        .ok_or_else(|| anyhow::anyhow!("Catalog '{}' not found in config", catalog_name))?;

    // D.5: Expand tilde in catalog path
    let expanded_path = shellexpand::tilde(&catalog_config.path);
    let repo_path = std::path::Path::new(expanded_path.as_ref());
    println!("Repository: {}", repo_path.display());
    println!("Type: {}", catalog_config.r#type);
    println!("Collection: {}", config.qdrant.collection);
    println!("Commit: {}", commit);
    println!();

    // Compute label_id
    let label_id = compute_label_id(catalog_name, label_name);
    println!("Label ID: {}", label_id);
    println!();

    // B.1: Load repo-specific crawl configuration
    let crawl_config = load_compiled_crawl_config(Some(repo_path))?;
    println!("Loaded crawl configuration for repository");

    // Initialize uploader
    let uploader = QdrantUploader::new(&config.qdrant.collection, config.qdrant.url.as_deref())?;

    // Step 1: Resolve commit to full SHA and write in-progress metadata
    println!("📦 Resolving commit...");
    let commit_oid = resolve_commit_oid(repo_path, commit)?;
    println!("Resolved {} to {}", commit, &commit_oid[..12]);

    // Write in-progress metadata before any work begins
    let in_progress_metadata = LabelMetadata {
        source_type: "label-metadata".to_string(),
        catalog: catalog_name.to_string(),
        label_id: label_id.clone(),
        label_name: label_name.to_string(),
        commit_oid: commit_oid.clone(),
        source_kind: "git-commit".to_string(),
        crawl_complete: false,
        updated_at_unix_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    };
    uploader.upsert_label_metadata(&in_progress_metadata)?;

    let files = enumerate_commit_tree(repo_path, commit)?;
    println!("Found {} files in commit tree", files.len());

    // Step 2: Build package index for this commit
    println!("📦 Building package index...");
    let package_index = build_package_index_for_commit(repo_path, commit)?;
    println!("Package index built successfully");
    println!();

    // Step 3: Filter files using crawl config (B.1: now uses repo-specific config)
    println!("📂 Filtering files...");
    let files_to_process: Vec<_> = files
        .iter()
        .filter(|f| crawl_config.should_crawl(&f.relative_path))
        .cloned()
        .collect();
    println!(
        "{} files to process after filtering",
        files_to_process.len()
    );
    println!();

    // Step 4: Process each file - check for existing chunks, then embed if needed
    println!("⚡ Phase 1: Checking existing chunks and collecting new files...");

    let mut new_files: Vec<(String, String)> = Vec::new(); // (relative_path, blob_id)
    let mut existing_files: HashSet<String> = HashSet::new();
    let mut new_count = 0;
    let mut existing_count = 0;

    for file_entry in &files_to_process {
        let file_id = compute_file_id(
            EMBEDDER_ID,
            CHUNKER_ID,
            &file_entry.blob_id,
            &file_entry.relative_path,
        );

        // Check if sentinel exists
        match uploader.get_file_sentinel(&file_id) {
            Ok(Some(_)) => {
                // File already indexed - just need to add label
                existing_files.insert(file_id);
                existing_count += 1;
            }
            Ok(None) => {
                // Need to index this file
                new_files.push((file_entry.relative_path.clone(), file_entry.blob_id.clone()));
                new_count += 1;
            }
            Err(e) => {
                eprintln!(
                    "  ⚠️  Error checking sentinel for {}: {}",
                    file_entry.relative_path, e
                );
                new_files.push((file_entry.relative_path.clone(), file_entry.blob_id.clone()));
                new_count += 1;
            }
        }
    }

    println!("  New files to index: {}", new_count);
    println!("  Existing files (label update only): {}", existing_count);
    println!();

    // Step 5: Add label to existing files
    // Track files that successfully got the label added
    let mut label_add_success_files: HashSet<String> = HashSet::new();
    if !existing_files.is_empty() {
        println!(
            "🏷️  Adding label to {} existing files...",
            existing_files.len()
        );
        for file_id in &existing_files {
            if let Err(e) = uploader.add_label_to_file_chunks(file_id, &label_id) {
                eprintln!("  ❌ Failed to add label to file {}: {}", file_id, e);
            } else {
                // Only track as successfully added after the call succeeds (A.3)
                label_add_success_files.insert(file_id.clone());
            }
        }
        println!("  Done.");
        println!();
    }
    // Replace existing_files with only the successful ones for cleanup logic
    let existing_files: HashSet<String> = label_add_success_files;

    // Step 6: Index new files
    let mut all_chunks: Vec<engine::Chunk> = Vec::new();
    let mut touched_file_ids: HashSet<String> = HashSet::new();

    // A.1/A.4: Failure tracking - declared at function scope for cleanup logic
    let upload_failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let file_complete_failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let label_add_failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let embedding_failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    if !new_files.is_empty() {
        // Initialize parallel embedder
        println!("⚙️  Loading embedding model...");
        let embedder = ParallelEmbedder::new()?;
        println!();

        println!(
            "📦 Phase 2: Chunking and embedding {} new files...",
            new_count
        );

        for (idx, (relative_path, blob_id)) in new_files.iter().enumerate() {
            print!(
                "\r  Processing file {}/{} ({:.0}%)   ",
                idx + 1,
                new_count,
                ((idx + 1) as f64 / new_count as f64) * 100.0
            );
            std::io::Write::flush(&mut std::io::stdout())?;

            // Read content from Git blob
            let content = match read_blob_content(repo_path, blob_id) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "\n  ⚠️  Failed to read blob {} for {}: {}",
                        &blob_id[..8],
                        relative_path,
                        e
                    );
                    continue;
                }
            };

            let content_str = match String::from_utf8(content) {
                Ok(s) => s,
                Err(_) => {
                    // Skip binary files
                    continue;
                }
            };

            // Resolve package name
            let package_name = package_index
                .find_package_name(relative_path)
                .unwrap_or(catalog_name)
                .to_string();

            // Create chunk context
            let ctx = ChunkContext {
                catalog: catalog_name.to_string(),
                label_id: label_id.clone(),
                package_name,
                relative_path: relative_path.clone(),
                blob_id: blob_id.clone(),
                source_uri: format!("{}:{}", repo_path.display(), relative_path),
            };

            // Chunk the content
            match chunk_content(&content_str, &ctx, 6000) {
                Ok(chunks) => {
                    if !chunks.is_empty() {
                        touched_file_ids.insert(chunks[0].file_id.clone());
                    }
                    all_chunks.extend(chunks);
                }
                Err(e) => {
                    eprintln!("\n  ⚠️  Failed to chunk {}: {}", relative_path, e);
                }
            }
        }

        let total_chunks = all_chunks.len();
        println!("\n  Found {} chunks to embed", total_chunks);
        println!();

        if !all_chunks.is_empty() {
            // Phase 3: Parallel embedding
            println!(
                "⚡ Phase 3: Embedding {} chunks with {} parallel sessions...",
                total_chunks,
                embedder.num_workers()
            );
            println!("  (Checkpoints every 60s - safe to CTRL+C)");
            let embed_start = std::time::Instant::now();

            let (embed_tx, embed_rx): (
                Sender<(engine::Chunk, Vec<f32>)>,
                Receiver<(engine::Chunk, Vec<f32>)>,
            ) = unbounded();
            let processed = Arc::new(AtomicUsize::new(0));
            let stop_flag = Arc::new(AtomicBool::new(false));
            let last_upload_time = Arc::new(Mutex::new(std::time::Instant::now()));

            // Progress reporter thread
            let processed_clone = Arc::clone(&processed);
            let stop_clone = Arc::clone(&stop_flag);
            let total_chunks_for_thread = total_chunks;
            let embed_start_for_thread = std::time::Instant::now();
            let last_print_time = Arc::new(Mutex::new(std::time::Instant::now()));
            let last_print_clone = Arc::clone(&last_print_time);

            let progress_thread = std::thread::spawn(move || {
                while !stop_clone.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    let mut last = last_print_clone.lock().unwrap();
                    if last.elapsed() >= std::time::Duration::from_secs(30) {
                        let current = processed_clone.load(Ordering::Relaxed);
                        let elapsed = embed_start_for_thread.elapsed();
                        let rate = current as f64 / elapsed.as_secs_f64().max(0.001);
                        let remaining = (total_chunks_for_thread - current) as f64 / rate;
                        let eta = format_eta(remaining);
                        eprintln!(
                            "[{}] Embedded {}/{} ({:.0}%) - {:.1} chunks/sec - ETA: {}",
                            chrono_timestamp(),
                            current,
                            total_chunks_for_thread,
                            (current as f64 / total_chunks_for_thread as f64) * 100.0,
                            rate,
                            eta
                        );
                        *last = std::time::Instant::now();
                    }
                }
            });

            // Wrap uploader in Arc<Mutex>
            let uploader = Arc::new(Mutex::new(uploader));

            // Uploader thread (failure tracking vars already declared at function scope)
            let stop_uploader = Arc::clone(&stop_flag);
            let last_upload_time_clone = Arc::clone(&last_upload_time);
            let uploader_clone = Arc::clone(&uploader);
            let label_id_clone = label_id.clone();
            let upload_failures_clone = Arc::clone(&upload_failures);
            let file_complete_failures_clone = Arc::clone(&file_complete_failures);
            let label_add_failures_clone = Arc::clone(&label_add_failures);

            let uploader_thread = std::thread::spawn(move || {
                let mut accumulated: Vec<(engine::Chunk, Vec<f32>)> = Vec::new();
                let mut expected_count: HashMap<String, usize> = HashMap::new();
                let mut uploaded_count: HashMap<String, usize> = HashMap::new();

                loop {
                    let should_upload = {
                        let mut last = last_upload_time_clone.lock().unwrap();
                        if last.elapsed() >= std::time::Duration::from_secs(60) {
                            *last = std::time::Instant::now();
                            true
                        } else {
                            false
                        }
                    };

                    while let Ok(embedded) = embed_rx.try_recv() {
                        let file_id = embedded.0.file_id.clone();
                        if let std::collections::hash_map::Entry::Vacant(e) =
                            expected_count.entry(file_id.clone())
                        {
                            e.insert(embedded.0.chunk_count);
                        }
                        accumulated.push(embedded);
                    }

                    if should_upload && !accumulated.is_empty() {
                        let count = accumulated.len();
                        eprintln!(
                            "[{}] Uploading checkpoint ({} chunks)...",
                            chrono_timestamp(),
                            count
                        );

                        let uploader_guard = uploader_clone.lock().unwrap();
                        match uploader_guard.upload_batch(&accumulated) {
                            Err(e) => {
                                eprintln!("[{}] ❌ Upload failed: {}", chrono_timestamp(), e);
                                // Track which files failed
                                let mut failures = upload_failures_clone.lock().unwrap();
                                for (chunk, _) in &accumulated {
                                    let file_id = &chunk.file_id;
                                    if !failures.contains(file_id) {
                                        failures.push(format!("{}: {}", file_id, e));
                                    }
                                }
                            }
                            Ok(_) => {
                                let mut files_in_batch: HashMap<String, usize> = HashMap::new();
                                for (chunk, _) in &accumulated {
                                    *files_in_batch.entry(chunk.file_id.clone()).or_insert(0) += 1;
                                }
                                for (file_id, batch_count) in &files_in_batch {
                                    *uploaded_count.entry(file_id.clone()).or_insert(0) +=
                                        batch_count;
                                }

                                let mut completed_files: Vec<String> = Vec::new();
                                for file_id in files_in_batch.keys() {
                                    let uploaded =
                                        uploaded_count.get(file_id).copied().unwrap_or(0);
                                    let expected =
                                        expected_count.get(file_id).copied().unwrap_or(0);
                                    if uploaded == expected && expected > 0 {
                                        completed_files.push(file_id.clone());
                                    }
                                }

                                for file_id in &completed_files {
                                    if let Err(e) = uploader_guard.mark_file_complete(file_id) {
                                        eprintln!(
                                            "[{}] ❌ Failed to mark file complete: {}",
                                            chrono_timestamp(),
                                            e
                                        );
                                        file_complete_failures_clone
                                            .lock()
                                            .unwrap()
                                            .push(format!("{}: {}", file_id, e));
                                    }
                                    // Add label to the file
                                    if let Err(e) = uploader_guard
                                        .add_label_to_file_chunks(file_id, &label_id_clone)
                                    {
                                        eprintln!(
                                            "[{}] ❌ Failed to add label: {}",
                                            chrono_timestamp(),
                                            e
                                        );
                                        label_add_failures_clone
                                            .lock()
                                            .unwrap()
                                            .push(format!("{}: {}", file_id, e));
                                    }
                                }
                            }
                        }
                        accumulated.clear();
                    }

                    if stop_uploader.load(Ordering::Relaxed) && embed_rx.is_empty() {
                        // Final upload
                        if !accumulated.is_empty() {
                            let count = accumulated.len();
                            eprintln!(
                                "[{}] Final upload ({} chunks)...",
                                chrono_timestamp(),
                                count
                            );

                            let uploader_guard = uploader_clone.lock().unwrap();
                            match uploader_guard.upload_batch(&accumulated) {
                                Ok(_) => {
                                    let mut files_in_batch: HashMap<String, usize> = HashMap::new();
                                    for (chunk, _) in &accumulated {
                                        *files_in_batch
                                            .entry(chunk.file_id.clone())
                                            .or_insert(0) += 1;
                                    }
                                    for file_id in files_in_batch.keys() {
                                        if let Err(e) = uploader_guard.mark_file_complete(file_id) {
                                            eprintln!(
                                                "[{}] ❌ Failed to mark file complete: {}",
                                                chrono_timestamp(),
                                                e
                                            );
                                            file_complete_failures_clone
                                                .lock()
                                                .unwrap()
                                                .push(format!("{}: {}", file_id, e));
                                        }
                                        if let Err(e) = uploader_guard
                                            .add_label_to_file_chunks(file_id, &label_id_clone)
                                        {
                                            eprintln!(
                                                "[{}] ❌ Failed to add label: {}",
                                                chrono_timestamp(),
                                                e
                                            );
                                            label_add_failures_clone
                                                .lock()
                                                .unwrap()
                                                .push(format!("{}: {}", file_id, e));
                                        }
                                    }
                                }
                                Err(e) => {
                                    eprintln!(
                                        "[{}] ❌ Final upload failed: {}",
                                        chrono_timestamp(),
                                        e
                                    );
                                    let mut failures = upload_failures_clone.lock().unwrap();
                                    for (chunk, _) in &accumulated {
                                        let file_id = &chunk.file_id;
                                        if !failures.iter().any(|f| f.starts_with(file_id)) {
                                            failures.push(format!("{}: {}", file_id, e));
                                        }
                                    }
                                }
                            }
                        }
                        break;
                    }

                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            });

            // Parallel embedding (A.5: use proper worker_index, A.4: track embedding failures)
            let processed_clone = Arc::clone(&processed);
            let embedding_failures_clone = Arc::clone(&embedding_failures);
            let num_workers = embedder.num_workers();

            all_chunks
                .into_par_iter()
                .enumerate()
                .for_each(|(idx, chunk)| {
                    let worker_index = idx % num_workers;
                    match embedder.encode(&chunk.text, worker_index) {
                        Ok(embedding) => {
                            let _ = embed_tx.send((chunk, embedding));
                            processed_clone.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            // A.4: Log embedding failures (not silently dropped)
                            eprintln!(
                                "\n[{}] ❌ Embedding failed for {}:{} - {}",
                                chrono_timestamp(),
                                chunk.relative_path,
                                chunk.chunk_ordinal,
                                e
                            );
                            let mut failures = embedding_failures_clone.lock().unwrap();
                            failures.push(format!(
                                "{}:{}: {}",
                                chunk.relative_path, chunk.chunk_ordinal, e
                            ));
                        }
                    }
                });

            // Signal completion
            stop_flag.store(true, Ordering::Relaxed);
            progress_thread.join().ok();
            uploader_thread.join().ok();

            let embed_elapsed = embed_start.elapsed();
            let rate = total_chunks as f64 / embed_elapsed.as_secs_f64().max(0.001);
            println!(
                "\n  Embedding complete: {} chunks in {} ({:.1} chunks/sec)",
                total_chunks,
                format_duration(embed_elapsed.as_secs_f64()),
                rate
            );

            // Report any failures from embedding/upload pipeline
            let upload_failures_count = upload_failures.lock().unwrap().len();
            let file_complete_failures_count = file_complete_failures.lock().unwrap().len();
            let label_add_failures_count = label_add_failures.lock().unwrap().len();
            let embedding_failures_count = embedding_failures.lock().unwrap().len();

            let total_failures = upload_failures_count
                + file_complete_failures_count
                + label_add_failures_count
                + embedding_failures_count;

            if total_failures > 0 {
                println!();
                println!(
                    "  ⚠️  Encountered {} embedding failures, {} upload failures, {} file-complete failures, {} label-add failures",
                    embedding_failures_count,
                    upload_failures_count,
                    file_complete_failures_count,
                    label_add_failures_count
                );
                println!("      These files may not be searchable. Check logs above for details.");
            }
            println!();
        }
    }

    // A.1: Compute total failures across all phases
    let total_failures = upload_failures.lock().unwrap().len()
        + file_complete_failures.lock().unwrap().len()
        + label_add_failures.lock().unwrap().len()
        + embedding_failures.lock().unwrap().len();
    let had_failures = total_failures > 0;

    // Step 7: Label reassignment cleanup (A.1: ONLY after fully successful crawl)
    // Remove label from chunks that were NOT touched in this crawl
    if had_failures {
        println!("🧹 Phase 4: SKIPPING label reassignment cleanup (crawl had failures)");
        println!("  This is intentional - cleanup should only run after successful crawls.");
        println!("  Run the crawl again to complete indexing and trigger cleanup.");
    } else {
        println!("🧹 Phase 4: Label reassignment cleanup...");
        let all_touched: HashSet<String> =
            existing_files.union(&touched_file_ids).cloned().collect();

        // Create a new uploader for cleanup (the previous one was moved into the uploader thread)
        let cleanup_uploader =
            QdrantUploader::new(&config.qdrant.collection, config.qdrant.url.as_deref())?;
        match cleanup_uploader.remove_label_from_chunks(&label_id, &all_touched) {
            Ok(processed) => {
                println!("  Processed {} chunks for label cleanup", processed);
            }
            Err(e) => {
                eprintln!("  ⚠️ Label cleanup failed: {}", e);
            }
        }
    }
    println!();

    // Step 8: Update label metadata (A.1: set crawl_complete=false if failures occurred)
    println!("📝 Updating label metadata...");
    let crawl_complete = !had_failures;
    let metadata = LabelMetadata {
        source_type: "label-metadata".to_string(),
        catalog: catalog_name.to_string(),
        label_id: label_id.clone(),
        label_name: label_name.to_string(),
        commit_oid: commit_oid.clone(),
        source_kind: "git-commit".to_string(),
        crawl_complete,
        updated_at_unix_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    };

    // Get uploader back from Arc<Mutex>
    // Note: This is a bit awkward - we need to get the uploader back
    // For now, create a new one
    let uploader = QdrantUploader::new(&config.qdrant.collection, config.qdrant.url.as_deref())?;
    uploader.upsert_label_metadata(&metadata)?;
    if crawl_complete {
        println!("  Label metadata saved.");
    } else {
        println!("  Label metadata saved (crawl_complete=false due to failures).");
    }
    println!();

    // Summary
    let total_elapsed = total_start.elapsed();
    if had_failures {
        println!("⚠️  Crawl completed with errors!");
        println!(
            "  Total time: {}",
            format_duration(total_elapsed.as_secs_f64())
        );
        println!("  New files indexed: {}", new_count);
        println!("  Existing files detected: {}", existing_count);
        println!(
            "  Existing files updated successfully: {}",
            existing_files.len()
        );
        println!("  Total failures: {}", total_failures);
        println!();
        println!("  This crawl is marked as incomplete. Re-run to complete indexing.");
    } else {
        println!("✅ Crawl complete!");
        println!(
            "  Total time: {}",
            format_duration(total_elapsed.as_secs_f64())
        );
        println!("  New files indexed: {}", new_count);
        println!("  Existing files detected: {}", existing_count);
        println!(
            "  Existing files updated successfully: {}",
            existing_files.len()
        );
    }

    // Report any critical failures (these are captured during the embed phase)
    // Note: upload_failures, file_complete_failures, label_add_failures are only
    // populated inside the embedder branch, so we need to handle the case where
    // they don't exist. For now, we track failures inline during processing.

    Ok(())
}

/// Run crawl for working directory (indexes uncommitted changes)
fn run_crawl_working_dir(
    config: &Config,
    catalog_name: &str,
    label_name: &str,
    _incremental_warnings: bool,
) -> anyhow::Result<()> {
    use crossbeam_channel::{Receiver, Sender, unbounded};
    use engine::util::{CHUNKER_ID, EMBEDDER_ID, compute_file_id};
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    let total_start = std::time::Instant::now();
    println!("🔍 Starting working directory crawl...");
    println!("Catalog: {}", catalog_name);
    println!("Label: {}", label_name);

    // Get catalog config
    let catalog_config = config
        .catalogs
        .get(catalog_name)
        .ok_or_else(|| anyhow::anyhow!("Catalog '{}' not found in config", catalog_name))?;

    // D.5: Expand tilde in catalog path
    let expanded_path = shellexpand::tilde(&catalog_config.path);
    let repo_path = std::path::Path::new(expanded_path.as_ref());
    println!("Repository: {}", repo_path.display());
    println!("Type: {}", catalog_config.r#type);
    println!("Collection: {}", config.qdrant.collection);
    println!("Source: working directory (uncommitted changes)");
    println!();

    // Compute label_id
    let label_id = compute_label_id(catalog_name, label_name);
    println!("Label ID: {}", label_id);
    println!();

    // B.1: Load repo-specific crawl configuration
    let crawl_config = load_compiled_crawl_config(Some(repo_path))?;
    println!("Loaded crawl configuration for repository");

    // Initialize uploader
    let uploader = QdrantUploader::new(&config.qdrant.collection, config.qdrant.url.as_deref())?;

    // Write in-progress metadata
    let in_progress_metadata = LabelMetadata {
        source_type: "label-metadata".to_string(),
        catalog: catalog_name.to_string(),
        label_id: label_id.clone(),
        label_name: label_name.to_string(),
        commit_oid: "".to_string(), // No commit for working directory
        source_kind: "working-directory".to_string(),
        crawl_complete: false,
        updated_at_unix_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    };
    uploader.upsert_label_metadata(&in_progress_metadata)?;

    // Enumerate working directory files
    println!("📂 Enumerating working directory...");
    let files = enumerate_working_directory(repo_path)?;
    println!(
        "Found {} files in working directory (before crawl config filtering)",
        files.len()
    );
    println!();

    // Build package index from working directory
    println!("📦 Building package index...");
    let package_index = build_package_index_for_working_dir(repo_path)?;
    println!("Package index built successfully");
    println!();

    // Filter files using compiled crawl config
    println!("📂 Filtering files...");
    let files_to_process: Vec<_> = files
        .iter()
        .filter(|f| crawl_config.should_crawl(&f.relative_path))
        .cloned()
        .collect();
    println!(
        "{} files to process after filtering",
        files_to_process.len()
    );
    println!();

    // Check for existing chunks and collect new files
    println!("⚡ Phase 1: Checking existing chunks and collecting new files...");

    let mut new_files: Vec<(String, String)> = Vec::new(); // (relative_path, content_hash)
    let mut existing_files: HashSet<String> = HashSet::new();
    let mut new_count = 0;
    let mut existing_count = 0;

    for file_entry in &files_to_process {
        // For working directory, use content_hash as blob_id
        let file_id = compute_file_id(
            EMBEDDER_ID,
            CHUNKER_ID,
            &file_entry.content_hash,
            &file_entry.relative_path,
        );

        match uploader.get_file_sentinel(&file_id) {
            Ok(Some(_)) => {
                existing_files.insert(file_id);
                existing_count += 1;
            }
            Ok(None) => {
                new_files.push((
                    file_entry.relative_path.clone(),
                    file_entry.content_hash.clone(),
                ));
                new_count += 1;
            }
            Err(e) => {
                eprintln!(
                    "  ⚠️  Error checking sentinel for {}: {}",
                    file_entry.relative_path, e
                );
                new_files.push((
                    file_entry.relative_path.clone(),
                    file_entry.content_hash.clone(),
                ));
                new_count += 1;
            }
        }
    }

    println!("  New files to index: {}", new_count);
    println!("  Existing files (label update only): {}", existing_count);
    println!();

    // Add label to existing files
    // A.3: Track files that successfully got the label added
    let mut label_add_success_files: HashSet<String> = HashSet::new();
    if !existing_files.is_empty() {
        println!(
            "🏷️  Adding label to {} existing files...",
            existing_files.len()
        );
        for file_id in &existing_files {
            if let Err(e) = uploader.add_label_to_file_chunks(file_id, &label_id) {
                eprintln!("  ❌ Failed to add label to file {}: {}", file_id, e);
            } else {
                label_add_success_files.insert(file_id.clone());
            }
        }
        println!("  Done.");
        println!();
    }
    // Replace existing_files with only the successful ones
    let existing_files: HashSet<String> = label_add_success_files;

    // Index new files
    let mut all_chunks: Vec<engine::Chunk> = Vec::new();
    let mut touched_file_ids: HashSet<String> = HashSet::new();

    // A.1/A.4: Failure tracking - declared at function scope for cleanup logic
    let upload_failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let file_complete_failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let label_add_failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let embedding_failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    if !new_files.is_empty() {
        println!("⚙️  Loading embedding model...");
        let embedder = ParallelEmbedder::new()?;
        println!();

        println!(
            "📦 Phase 2: Chunking and embedding {} new files...",
            new_count
        );

        for (idx, (relative_path, content_hash)) in new_files.iter().enumerate() {
            print!(
                "\r  Processing file {}/{} ({:.0}%)   ",
                idx + 1,
                new_count,
                ((idx + 1) as f64 / new_count as f64) * 100.0
            );
            std::io::Write::flush(&mut std::io::stdout())?;

            // Read content from working directory
            let content = match read_working_file_content(repo_path, relative_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("\n  ⚠️  Failed to read {}: {}", relative_path, e);
                    continue;
                }
            };

            let content_str = match String::from_utf8(content) {
                Ok(s) => s,
                Err(_) => continue,
            };

            // Resolve package name
            let package_name = package_index
                .find_package_name(relative_path)
                .unwrap_or(catalog_name)
                .to_string();

            // Create chunk context - use content_hash as blob_id
            let ctx = ChunkContext {
                catalog: catalog_name.to_string(),
                label_id: label_id.clone(),
                package_name,
                relative_path: relative_path.clone(),
                blob_id: content_hash.clone(), // Use content hash as blob_id
                source_uri: format!("{}:{}", repo_path.display(), relative_path),
            };

            match chunk_content(&content_str, &ctx, 6000) {
                Ok(chunks) => {
                    if !chunks.is_empty() {
                        touched_file_ids.insert(chunks[0].file_id.clone());
                    }
                    all_chunks.extend(chunks);
                }
                Err(e) => {
                    eprintln!("\n  ⚠️  Failed to chunk {}: {}", relative_path, e);
                }
            }
        }

        let total_chunks = all_chunks.len();
        println!("\n  Found {} chunks to embed", total_chunks);
        println!();

        if !all_chunks.is_empty() {
            // Embed and upload (same as commit-based crawl)
            println!(
                "⚡ Phase 3: Embedding {} chunks with {} parallel sessions...",
                total_chunks,
                embedder.num_workers()
            );
            println!("  (Checkpoints every 60s - safe to CTRL+C)");
            let embed_start = std::time::Instant::now();

            let (embed_tx, embed_rx): (
                Sender<(engine::Chunk, Vec<f32>)>,
                Receiver<(engine::Chunk, Vec<f32>)>,
            ) = unbounded();
            let processed = Arc::new(AtomicUsize::new(0));
            let stop_flag = Arc::new(AtomicBool::new(false));
            let last_upload_time = Arc::new(Mutex::new(std::time::Instant::now()));

            let processed_clone = Arc::clone(&processed);
            let stop_clone = Arc::clone(&stop_flag);
            let total_chunks_for_thread = total_chunks;
            let embed_start_for_thread = std::time::Instant::now();
            let last_print_clone = Arc::clone(&last_upload_time);

            let progress_thread = std::thread::spawn(move || {
                while !stop_clone.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    let mut last = last_print_clone.lock().unwrap();
                    if last.elapsed() >= std::time::Duration::from_secs(30) {
                        let current = processed_clone.load(Ordering::Relaxed);
                        let elapsed = embed_start_for_thread.elapsed();
                        let rate = current as f64 / elapsed.as_secs_f64().max(0.001);
                        let remaining = (total_chunks_for_thread - current) as f64 / rate;
                        let eta = format_eta(remaining);
                        eprintln!(
                            "[{}] Embedded {}/{} ({:.0}%) - {:.1} chunks/sec - ETA: {}",
                            chrono_timestamp(),
                            current,
                            total_chunks_for_thread,
                            (current as f64 / total_chunks_for_thread as f64) * 100.0,
                            rate,
                            eta
                        );
                        *last = std::time::Instant::now();
                    }
                }
            });

            let uploader = Arc::new(Mutex::new(uploader));
            let stop_uploader = Arc::clone(&stop_flag);
            let last_upload_time_clone = Arc::clone(&last_upload_time);
            let uploader_clone = Arc::clone(&uploader);
            let label_id_clone = label_id.clone();

            // Failure tracking clones (vars already declared at function scope)
            let upload_failures_clone = Arc::clone(&upload_failures);
            let file_complete_failures_clone = Arc::clone(&file_complete_failures);
            let label_add_failures_clone = Arc::clone(&label_add_failures);

            let uploader_thread = std::thread::spawn(move || {
                let mut accumulated: Vec<(engine::Chunk, Vec<f32>)> = Vec::new();
                let mut expected_count: HashMap<String, usize> = HashMap::new();
                let mut uploaded_count: HashMap<String, usize> = HashMap::new();

                loop {
                    let should_upload = {
                        let mut last = last_upload_time_clone.lock().unwrap();
                        if last.elapsed() >= std::time::Duration::from_secs(60) {
                            *last = std::time::Instant::now();
                            true
                        } else {
                            false
                        }
                    };

                    while let Ok(embedded) = embed_rx.try_recv() {
                        let file_id = embedded.0.file_id.clone();
                        if let std::collections::hash_map::Entry::Vacant(e) =
                            expected_count.entry(file_id.clone())
                        {
                            e.insert(embedded.0.chunk_count);
                        }
                        accumulated.push(embedded);
                    }

                    if should_upload && !accumulated.is_empty() {
                        let count = accumulated.len();
                        eprintln!(
                            "[{}] Uploading checkpoint ({} chunks)...",
                            chrono_timestamp(),
                            count
                        );

                        let uploader_guard = uploader_clone.lock().unwrap();
                        if let Err(e) = uploader_guard.upload_batch(&accumulated) {
                            eprintln!("[{}] ❌ Upload failed: {}", chrono_timestamp(), e);
                            // A.1: Track upload failures
                            let mut failures = upload_failures_clone.lock().unwrap();
                            for (chunk, _) in &accumulated {
                                let file_id = &chunk.file_id;
                                if !failures.iter().any(|f| f.starts_with(file_id)) {
                                    failures.push(format!("{}: {}", file_id, e));
                                }
                            }
                        } else {
                            let mut files_in_batch: HashMap<String, usize> = HashMap::new();
                            for (chunk, _) in &accumulated {
                                *files_in_batch.entry(chunk.file_id.clone()).or_insert(0) += 1;
                            }
                            for (file_id, batch_count) in &files_in_batch {
                                *uploaded_count.entry(file_id.clone()).or_insert(0) += batch_count;
                            }

                            let mut completed_files: Vec<String> = Vec::new();
                            for file_id in files_in_batch.keys() {
                                let uploaded = uploaded_count.get(file_id).copied().unwrap_or(0);
                                let expected = expected_count.get(file_id).copied().unwrap_or(0);
                                if uploaded == expected && expected > 0 {
                                    completed_files.push(file_id.clone());
                                }
                            }

                            for file_id in &completed_files {
                                if let Err(e) = uploader_guard.mark_file_complete(file_id) {
                                    eprintln!(
                                        "[{}] ❌ Failed to mark file complete: {}",
                                        chrono_timestamp(),
                                        e
                                    );
                                    // A.1: Track file complete failures
                                    file_complete_failures_clone
                                        .lock()
                                        .unwrap()
                                        .push(format!("{}: {}", file_id, e));
                                }
                                if let Err(e) = uploader_guard
                                    .add_label_to_file_chunks(file_id, &label_id_clone)
                                {
                                    eprintln!(
                                        "[{}] ❌ Failed to add label: {}",
                                        chrono_timestamp(),
                                        e
                                    );
                                    // A.1: Track label add failures
                                    label_add_failures_clone
                                        .lock()
                                        .unwrap()
                                        .push(format!("{}: {}", file_id, e));
                                }
                            }
                        }
                        accumulated.clear();
                    }

                    if stop_uploader.load(Ordering::Relaxed) && embed_rx.is_empty() {
                        if !accumulated.is_empty() {
                            let count = accumulated.len();
                            eprintln!(
                                "[{}] Final upload ({} chunks)...",
                                chrono_timestamp(),
                                count
                            );

                            let uploader_guard = uploader_clone.lock().unwrap();
                            if let Err(e) = uploader_guard.upload_batch(&accumulated) {
                                eprintln!("[{}] ❌ Final upload failed: {}", chrono_timestamp(), e);
                                // A.1: Track upload failures
                                let mut failures = upload_failures_clone.lock().unwrap();
                                for (chunk, _) in &accumulated {
                                    let file_id = &chunk.file_id;
                                    if !failures.iter().any(|f| f.starts_with(file_id)) {
                                        failures.push(format!("{}: {}", file_id, e));
                                    }
                                }
                            } else {
                                for file_id in accumulated
                                    .iter()
                                    .map(|(c, _)| c.file_id.clone())
                                    .collect::<std::collections::HashSet<_>>()
                                {
                                    if let Err(e) = uploader_guard.mark_file_complete(&file_id) {
                                        eprintln!(
                                            "[{}] ❌ Failed to mark file complete: {}",
                                            chrono_timestamp(),
                                            e
                                        );
                                        file_complete_failures_clone
                                            .lock()
                                            .unwrap()
                                            .push(format!("{}: {}", file_id, e));
                                    }
                                    if let Err(e) = uploader_guard
                                        .add_label_to_file_chunks(&file_id, &label_id_clone)
                                    {
                                        eprintln!(
                                            "[{}] ❌ Failed to add label: {}",
                                            chrono_timestamp(),
                                            e
                                        );
                                        label_add_failures_clone
                                            .lock()
                                            .unwrap()
                                            .push(format!("{}: {}", file_id, e));
                                    }
                                }
                            }
                        }
                        break;
                    }

                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            });

            // A.4, A.5: Track embedding failures and use proper worker_index
            let processed_clone = Arc::clone(&processed);
            let embedding_failures_clone = Arc::clone(&embedding_failures);
            let num_workers = embedder.num_workers();

            all_chunks
                .into_par_iter()
                .enumerate()
                .for_each(|(idx, chunk)| {
                    let worker_index = idx % num_workers;
                    match embedder.encode(&chunk.text, worker_index) {
                        Ok(embedding) => {
                            let _ = embed_tx.send((chunk, embedding));
                            processed_clone.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            eprintln!(
                                "\n[{}] ❌ Embedding failed for {}:{} - {}",
                                chrono_timestamp(),
                                chunk.relative_path,
                                chunk.chunk_ordinal,
                                e
                            );
                            let mut failures = embedding_failures_clone.lock().unwrap();
                            failures.push(format!(
                                "{}:{}: {}",
                                chunk.relative_path, chunk.chunk_ordinal, e
                            ));
                        }
                    }
                });

            stop_flag.store(true, Ordering::Relaxed);
            progress_thread.join().ok();
            uploader_thread.join().ok();

            let embed_elapsed = embed_start.elapsed();
            let rate = total_chunks as f64 / embed_elapsed.as_secs_f64().max(0.001);
            println!(
                "\n  Embedding complete: {} chunks in {} ({:.1} chunks/sec)",
                total_chunks,
                format_duration(embed_elapsed.as_secs_f64()),
                rate
            );

            // Report any failures from embedding/upload pipeline
            let upload_failures_count = upload_failures.lock().unwrap().len();
            let file_complete_failures_count = file_complete_failures.lock().unwrap().len();
            let label_add_failures_count = label_add_failures.lock().unwrap().len();
            let embedding_failures_count = embedding_failures.lock().unwrap().len();

            let total_failures = upload_failures_count
                + file_complete_failures_count
                + label_add_failures_count
                + embedding_failures_count;

            if total_failures > 0 {
                println!();
                println!(
                    "  ⚠️  Encountered {} embedding failures, {} upload failures, {} file-complete failures, {} label-add failures",
                    embedding_failures_count,
                    upload_failures_count,
                    file_complete_failures_count,
                    label_add_failures_count
                );
                println!("      These files may not be searchable. Check logs above for details.");
            }
            println!();
        }
    }

    // A.1: Compute total failures and skip cleanup if any occurred
    let total_failures = upload_failures.lock().unwrap().len()
        + file_complete_failures.lock().unwrap().len()
        + label_add_failures.lock().unwrap().len()
        + embedding_failures.lock().unwrap().len();
    let had_failures = total_failures > 0;

    // Label reassignment cleanup (A.1: ONLY after fully successful crawl)
    if had_failures {
        println!("🧹 Phase 4: SKIPPING label reassignment cleanup (crawl had failures)");
        println!("  This is intentional - cleanup should only run after successful crawls.");
        println!("  Run the crawl again to complete indexing and trigger cleanup.");
    } else {
        println!("🧹 Phase 4: Label reassignment cleanup...");
        let all_touched: HashSet<String> =
            existing_files.union(&touched_file_ids).cloned().collect();

        let cleanup_uploader =
            QdrantUploader::new(&config.qdrant.collection, config.qdrant.url.as_deref())?;
        match cleanup_uploader.remove_label_from_chunks(&label_id, &all_touched) {
            Ok(processed) => println!("  Processed {} chunks for label cleanup", processed),
            Err(e) => eprintln!("  ⚠️ Label cleanup failed: {}", e),
        }
    }
    println!();

    // Update label metadata (A.1: set crawl_complete=false if failures occurred)
    println!("📝 Updating label metadata...");
    let crawl_complete = !had_failures;
    let metadata = LabelMetadata {
        source_type: "label-metadata".to_string(),
        catalog: catalog_name.to_string(),
        label_id: label_id.clone(),
        label_name: label_name.to_string(),
        commit_oid: "".to_string(),
        source_kind: "working-directory".to_string(),
        crawl_complete,
        updated_at_unix_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    };

    let uploader = QdrantUploader::new(&config.qdrant.collection, config.qdrant.url.as_deref())?;
    uploader.upsert_label_metadata(&metadata)?;
    if crawl_complete {
        println!("  Label metadata saved.");
    } else {
        println!("  Label metadata saved (crawl_complete=false due to failures).");
    }
    println!();

    let total_elapsed = total_start.elapsed();
    if had_failures {
        println!("⚠️  Working directory crawl completed with errors!");
        println!(
            "  Total time: {}",
            format_duration(total_elapsed.as_secs_f64())
        );
        println!("  New files indexed: {}", new_count);
        println!("  Existing files detected: {}", existing_count);
        println!(
            "  Existing files updated successfully: {}",
            existing_files.len()
        );
        println!("  Total failures: {}", total_failures);
        println!();
        println!("  This crawl is marked as incomplete. Re-run to complete indexing.");
    } else {
        println!("✅ Working directory crawl complete!");
        println!(
            "  Total time: {}",
            format_duration(total_elapsed.as_secs_f64())
        );
        println!("  New files indexed: {}", new_count);
        println!("  Existing files detected: {}", existing_count);
        println!(
            "  Existing files updated successfully: {}",
            existing_files.len()
        );
    }

    Ok(())
}

/// Run search with compact blurb output
fn run_search(
    config: &Config,
    text: &str,
    limit: usize,
    label: Option<&str>,
    catalog: Option<&str>,
) -> anyhow::Result<()> {
    // Resolve label ID from explicit flag or default context
    let (label_id, _, _) = resolve_label_id(label, catalog)?;

    // Generate embedding for query
    let embedder = ParallelEmbedder::new()?;
    let embedding = embedder.encode(text, 0)?;

    // Query Qdrant with label filter
    let uploader = QdrantUploader::new(&config.qdrant.collection, config.qdrant.url.as_deref())?;

    // Extract catalog from label_id (format: catalog:label)
    let catalog = label_id.split(':').next().unwrap_or("");
    let results = uploader.search_with_label(&embedding, limit, catalog, &label_id)?;

    // Display results as blurbs
    for result in &results {
        // Line 1: file_id:chunk_ordinal  score  breadcrumb
        let breadcrumb = result.payload.breadcrumb.as_deref().unwrap_or("unknown");
        println!(
            "{}:{}  {:.3}  {}",
            result.payload.file_id, result.payload.chunk_ordinal, result.score, breadcrumb
        );

        // Lines 2-4: first 3 lines of code (quoted with >)
        for line in result.payload.text.lines().take(3) {
            println!("> {}", line);
        }

        // Blank line between results
        println!();
    }

    Ok(())
}

/// Run view command to display full chunks by IDs
/// Parsed selector for file-based chunk queries
#[derive(Debug, Clone)]
enum ChunkSelector {
    /// All chunks in the file
    All,
    /// Single chunk at position N (1-indexed)
    Single(usize),
    /// Range from start to end (inclusive, 1-indexed)
    Range(usize, usize),
    /// Range from start to the end of file
    ToEnd(usize),
}

/// Parse file ID with optional selector
///
/// Formats:
/// - `700a4ba232fe9ddc` - all chunks in file
/// - `700a4ba232fe9ddc:3` - chunk 3
/// - `700a4ba232fe9ddc:2-3` - chunks 2 through 3
/// - `700a4ba232fe9ddc:3-end` - chunk 3 through the last chunk
fn parse_file_id_with_selector(s: &str) -> anyhow::Result<(String, ChunkSelector)> {
    let s = s.trim();

    // Check for selector suffix
    if let Some(colon_pos) = s.find(':') {
        let file_id = s[..colon_pos].to_string();
        let selector = &s[colon_pos + 1..];

        // Validate file_id is 16 hex chars
        if file_id.len() != 16 || !file_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(anyhow::anyhow!(
                "Invalid file ID '{}'. Expected 16 hex characters.",
                file_id
            ));
        }

        // Parse selector
        if selector == "end" {
            // Invalid: ":end" without start
            return Err(anyhow::anyhow!(
                "Invalid selector ':end'. Use ':N-end' format."
            ));
        } else if selector.ends_with("-end") {
            // :N-end format
            let start_str = &selector[..selector.len() - 4];
            let start: usize = start_str
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid chunk number in selector '{}'", selector))?;
            if start < 1 {
                return Err(anyhow::anyhow!(
                    "Chunk numbers are 1-indexed, got {}",
                    start
                ));
            }
            Ok((file_id, ChunkSelector::ToEnd(start)))
        } else if selector.contains('-') {
            // :N-M format
            let parts: Vec<&str> = selector.split('-').collect();
            if parts.len() != 2 {
                return Err(anyhow::anyhow!(
                    "Invalid selector '{}'. Expected ':N-M' format.",
                    selector
                ));
            }
            let start: usize = parts[0]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid start chunk in selector '{}'", selector))?;
            let end: usize = parts[1]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid end chunk in selector '{}'", selector))?;
            if start < 1 || end < 1 {
                return Err(anyhow::anyhow!(
                    "Chunk numbers are 1-indexed, got {}:{}",
                    start,
                    end
                ));
            }
            if start > end {
                return Err(anyhow::anyhow!("Start chunk {} > end chunk {}", start, end));
            }
            Ok((file_id, ChunkSelector::Range(start, end)))
        } else {
            // :N format (single chunk)
            let chunk_num: usize = selector
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid chunk number in selector '{}'", selector))?;
            if chunk_num < 1 {
                return Err(anyhow::anyhow!(
                    "Chunk numbers are 1-indexed, got {}",
                    chunk_num
                ));
            }
            Ok((file_id, ChunkSelector::Single(chunk_num)))
        }
    } else {
        // No selector - validate file_id and return All selector
        if s.len() != 16 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(anyhow::anyhow!(
                "Invalid file ID '{}'. Expected 16 hex characters.",
                s
            ));
        }
        Ok((s.to_string(), ChunkSelector::All))
    }
}

fn run_view(
    config: &Config,
    id_specs: &[String],
    label: Option<&str>,
    show_full_paths: bool,
    chunks_only: bool,
) -> anyhow::Result<()> {
    if id_specs.is_empty() {
        return Err(anyhow::anyhow!(
            "No IDs provided. Use --id <file_id>[:<selector>]"
        ));
    }

    // Resolve label ID from explicit flag or default context
    let (label_id, _, _) = resolve_label_id(label, None)?;

    // Parse all file IDs with selectors
    let mut requests: Vec<(String, ChunkSelector)> = Vec::new();
    for spec in id_specs {
        let (file_id, selector) = parse_file_id_with_selector(spec)?;
        requests.push((file_id, selector));
    }

    // Query Qdrant
    let uploader = QdrantUploader::new(&config.qdrant.collection, config.qdrant.url.as_deref())?;

    // Collect all results with their original selectors for display
    let mut all_results: Vec<(String, ChunkSelector, Vec<PointResult>)> = Vec::new();

    for (file_id, selector) in requests {
        let chunks = uploader.get_chunks_by_file_id_with_label(&file_id, &label_id)?;

        // Filter by selector
        let filtered: Vec<PointResult> = match &selector {
            ChunkSelector::All => chunks,
            ChunkSelector::Single(n) => chunks
                .into_iter()
                .filter(|c| c.payload.chunk_ordinal == *n)
                .collect(),
            ChunkSelector::Range(start, end) => chunks
                .into_iter()
                .filter(|c| c.payload.chunk_ordinal >= *start && c.payload.chunk_ordinal <= *end)
                .collect(),
            ChunkSelector::ToEnd(start) => chunks
                .into_iter()
                .filter(|c| c.payload.chunk_ordinal >= *start)
                .collect(),
        };

        all_results.push((file_id, selector, filtered));
    }

    // Collect unique catalogs for preamble
    if !chunks_only {
        let catalogs: std::collections::HashSet<&str> = all_results
            .iter()
            .flat_map(|(_, _, results)| results.iter().map(|r| r.payload.catalog.as_str()))
            .collect();

        if !catalogs.is_empty() {
            println!("Catalogs:");
            for cat in catalogs {
                if let Some(cat_config) = config.catalogs.get(cat) {
                    println!("- {}", cat);
                    println!("  Catalog path: {}", cat_config.path);
                }
            }
            println!();
        }
    }

    // Display results
    for (file_id, selector, results) in &all_results {
        if results.is_empty() {
            // No chunks found
            let selector_str = match selector {
                ChunkSelector::All => String::new(),
                ChunkSelector::Single(n) => format!(":{}", n),
                ChunkSelector::Range(start, end) => format!(":{}-{}", start, end),
                ChunkSelector::ToEnd(start) => format!(":{}-end", start),
            };
            println!("{}{} ERROR: CHUNK NOT FOUND", file_id, selector_str);
            continue;
        }

        for result in results {
            let breadcrumb = result.payload.breadcrumb.as_deref().unwrap_or("unknown");
            let chunk_count = result.payload.chunk_count;
            let chunk_ordinal = result.payload.chunk_ordinal;

            // Header line: <file_id>:<chunk_ordinal> (<n>/<total>) <breadcrumb>
            println!(
                "{}:{} ({}/{}) {}",
                file_id, chunk_ordinal, chunk_ordinal, chunk_count, breadcrumb
            );

            // Source line
            println!(
                "Source: {}:{}",
                result.payload.catalog, result.payload.relative_path
            );

            // Full path (optional)
            if show_full_paths {
                println!("Full path: {}", result.payload.source_uri);
            }

            // Lines and type
            println!(
                "Lines: {}-{}",
                result.payload.start_line, result.payload.end_line
            );
            println!("Type: {}", result.payload.chunk_type);

            // Content
            println!();
            for line in result.payload.text.lines() {
                println!("> {}", line);
            }

            println!();
        }
    }

    Ok(())
}

/// Run purge command (delete all chunks from a catalog or entire collection)
fn run_purge(config: &Config, catalog: Option<&str>, all: bool) -> anyhow::Result<()> {
    let uploader = QdrantUploader::new(&config.qdrant.collection, config.qdrant.url.as_deref())?;

    if all {
        println!(
            "🗑️  Purging entire collection: {}",
            config.qdrant.collection
        );
        println!("This will delete ALL data from the collection!");

        // Delete all points with empty filter
        let endpoint = format!(
            "{}/collections/{}/points/delete",
            config
                .qdrant
                .url
                .as_deref()
                .unwrap_or("http://localhost:6333"),
            config.qdrant.collection
        );

        let empty_filter = serde_json::json!({"filter": {}});

        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            .json(&empty_filter)
            .send()?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to purge collection: HTTP {}",
                response.status()
            ));
        }

        println!("✅ Collection purged successfully");
    } else if let Some(catalog_name) = catalog {
        println!("🗑️  Purging catalog: {}", catalog_name);

        let operation_id = uploader.delete_catalog(catalog_name)?;
        println!(
            "✅ Catalog purged successfully (operation ID: {})",
            operation_id
        );
    } else {
        return Err(anyhow::anyhow!(
            "Must specify either --catalog <name> or --all"
        ));
    }

    Ok(())
}

/// Run chunking diagnostics on a TypeScript file
fn run_dump_chunks(
    file: &PathBuf,
    target_size: usize,
    visualize: bool,
    with_fallback: bool,
    enable_debug: bool,
) -> anyhow::Result<()> {
    println!("📦 Chunks for: {}", file.display());
    if !with_fallback {
        println!("🔍 Strict mode: AST-only (fallback disabled)");
    }
    println!();

    // Read file
    let source =
        std::fs::read_to_string(file).map_err(|e| anyhow::anyhow!("Failed to read file: {}", e))?;

    // Determine file name and package name
    let file_name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown.ts");

    // Find package name by walking upward to find nearest package.json
    let file_path = file.to_string_lossy().to_string();
    let package_name = engine::package_lookup::find_package_name(&file_path, "");

    // Create config
    let config = PartitionConfig {
        target_size,
        file_name: file_name.to_string(),
        package_name: package_name.clone(),
        debug: PartitionDebug {
            enabled: enable_debug,
        },
        allow_fallback: with_fallback, // AST-only by default, enable fallback with flag
    };

    // Partition
    let chunks = partition_typescript(&source, &config, &file_path, &package_name);

    // Quality score
    let file_chars = source.len();
    let report = ChunkQualityReport::from_chunks(&chunks, file_chars);

    if visualize {
        // Visualization mode: show full chunk contents
        let lines: Vec<&str> = source.lines().collect();

        for (i, chunk) in chunks.iter().enumerate() {
            let line_count = chunk.end_line - chunk.start_line + 1;
            let size = chunk.text.len();

            println!(
                "-- [CHUNK {}] [{} lines] [{} chars] --",
                i + 1,
                line_count,
                size
            );

            for line_num in chunk.start_line..=chunk.end_line {
                if line_num > 0 && line_num <= lines.len() {
                    println!("{}", lines[line_num - 1]);
                }
            }
            println!();
        }

        println!("=== QUALITY SCORE ===");
        println!("Score: {:.1}%", report.score);
        println!("Total chunks: {}", chunks.len());
        println!(
            "Small chunks (<{} chars): {}",
            SMALL_CHUNK_CHARS, report.small_chunks
        );
        println!(
            "Chars: {}-{} (mean {:.0})",
            report.min_chars, report.max_chars, report.mean_chars
        );
    } else {
        // Default mode: show summary with previews
        println!("Total chunks: {}", chunks.len());
        println!("Target size: {} chars", target_size);
        println!();

        let mut total_chars = 0;
        let mut oversized = 0;
        let mut undersized = 0;

        for (i, chunk) in chunks.iter().enumerate() {
            let text_size = chunk.text.len();
            let total_size = chunk.breadcrumb.len() + chunk.text.len();
            total_chars += total_size;

            if text_size > target_size {
                oversized += 1;
            } else if text_size < 200 {
                undersized += 1;
            }

            println!("━━━━━ Chunk {} ━━━━━", i + 1);
            println!("Breadcrumb: {}", chunk.breadcrumb);
            println!("Type: {}", chunk.chunk_type);
            if let Some(symbol) = &chunk.symbol_name {
                println!("Symbol: {}", symbol);
            }
            println!("Lines: {}-{}", chunk.start_line, chunk.end_line);
            println!(
                "Size: {} chars (text: {}, breadcrumb: {})",
                total_size,
                text_size,
                chunk.breadcrumb.len()
            );
            if text_size > target_size {
                println!(
                    "⚠️  OVERSIZED (target: {}, actual: {})",
                    target_size, text_size
                );
            } else if text_size < 200 {
                println!("⚡ Small chunk");
            }
            println!();
            println!("Preview (first 8 lines):");
            for line in chunk.text.lines().take(8) {
                println!("  {}", line);
            }
            if chunk.text.lines().count() > 8 {
                println!("  ... ({} more lines)", chunk.text.lines().count() - 8);
            }
            println!();
        }

        println!("━━━━━ Summary ━━━━━");
        println!("Total chunks: {}", chunks.len());
        println!("Total chars: {}", total_chars);
        println!(
            "Average size: {:.0} chars",
            total_chars as f64 / chunks.len() as f64
        );
        println!("Oversized chunks (>{}): {}", target_size, oversized);
        println!("Small chunks (<200): {}", undersized);
        println!("Quality score: {:.1}%", report.score);
        println!(
            "  Small chunks (<{} chars): {}",
            SMALL_CHUNK_CHARS, report.small_chunks
        );
    }

    Ok(())
}

/// Audit chunking quality across multiple files
fn run_audit_chunks(count: usize, dir: String) -> anyhow::Result<()> {
    use rand::seq::IndexedRandom;

    println!("📊 Sampling {} TypeScript files from: {}", count, dir);
    println!();

    // Collect all TypeScript files
    let ts_files: Vec<PathBuf> = walkdir::WalkDir::new(&dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let path = e.path();
            let ext = path
                .extension()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default();
            ext == "ts" && !path.to_string_lossy().contains("node_modules")
        })
        .map(|e| e.path().to_owned())
        .collect();

    println!("Found {} TypeScript files", ts_files.len());

    if ts_files.is_empty() {
        return Err(anyhow::anyhow!("No TypeScript files found"));
    }

    // Random sample
    let mut rng = rand::rng();
    let sample: Vec<_> = ts_files.sample(&mut rng, count).into_iter().collect();

    // Compute quality scores using AST-only mode (allow_fallback=false)
    // This measures how well the AST-based chunker performs, without fallback
    // masking the quality of split decisions.
    let mut results: Vec<_> = sample
        .into_iter()
        .filter_map(|path| {
            let source = std::fs::read_to_string(&path).ok()?;
            let file_name = path.file_name()?.to_string_lossy().to_string();
            let config = PartitionConfig {
                file_name,
                package_name: "n/a".to_string(),
                allow_fallback: false, // AST-only mode for accurate quality measurement
                ..Default::default()
            };
            let chunks = partition_typescript(&source, &config, path.to_str().unwrap(), "n/a");
            let file_chars = source.len();
            let report = ChunkQualityReport::from_chunks(&chunks, file_chars);
            Some((path, report, chunks))
        })
        .collect();

    // Sort by score (worst first - ascending since higher is better)
    results.sort_by(|a, b| a.1.score.partial_cmp(&b.1.score).unwrap());

    println!("\n=== Quality Scores (worst first) ===\n");
    for (i, (path, report, _)) in results.iter().enumerate() {
        let rel_path = path.strip_prefix(&dir).unwrap_or(path);
        println!("{}. {} {}", i + 1, report.format(), rel_path.display());
    }

    // Show top 3 worst for investigation
    println!("\n=== Top 3 Worst Files ===\n");
    for (path, report, chunks) in results.iter().take(3) {
        let rel_path = path.strip_prefix(&dir).unwrap_or(path);
        println!("--- {} ---", rel_path.display());
        println!("{}", report.format());

        // Show chunk breakdown
        for (i, chunk) in chunks.iter().enumerate() {
            let lines = chunk.end_line - chunk.start_line + 1;
            let tiny_marker = if lines < 20 { " [TINY]" } else { "" };
            println!(
                "  Chunk {}: {} lines ({}-{}) {} - {}",
                i + 1,
                lines,
                chunk.start_line,
                chunk.end_line,
                tiny_marker,
                chunk.breadcrumb
            );
        }
        println!();
    }

    Ok(())
}
