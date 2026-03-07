//! rush-qdrant: Semantic search indexer for Rush monorepos
//! 
//! Uses Qdrant vector database with jina-embeddings-v2-base-code embeddings
//! Intelligently chunks code and documentation for high-quality semantic search

mod engine;

use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::path::PathBuf;
use engine::{
    config::should_skip_path,
    chunker::chunk_file,
    ParallelEmbedder,
    partitioner::{partition_typescript, PartitionConfig},
    uploader::QdrantUploader,
};

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
#[command(name = "rush-qdrant", version, about)]
struct Cli {
    /// Config file path (default: ~/.config/rush-qdrant/config.jsonc)
    #[arg(long)]
    config: Option<PathBuf>,
    
    #[command(subcommand)]
    command: Commands,
}

/// Available commands
#[derive(Subcommand)]
enum Commands {
    /// Crawl source and index into Qdrant (incremental sync)
    Crawl {
        /// Catalog name (from config file)
        #[arg(long)]
        catalog: String,
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
    
    /// Dump chunks for a TypeScript file (for debugging chunking algorithm)
    DumpChunks {
        /// TypeScript file path
        #[arg(long)]
        file: PathBuf,
        
        /// Target chunk size in chars
        #[arg(long, default_value = "1800")]
        target_size: usize,
    },
    
    /// Query the semantic search database
    Query {
        /// Search query text
        #[arg(long)]
        text: String,
        
        /// Number of results
        #[arg(long, default_value = "5")]
        limit: usize,
        
        /// Filter by catalog (optional - searches all if omitted)
        #[arg(long)]
        catalog: Option<String>,
    },
}

const DEFAULT_CONFIG_PATH: &str = "~/.config/rush-qdrant/config.jsonc";

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
    let config_path = cli.config.unwrap_or_else(|| {
        PathBuf::from(shellexpand::tilde(DEFAULT_CONFIG_PATH).as_ref())
    });
    let config = load_config(&config_path)?;

    match cli.command {
        Commands::Crawl { catalog } => {
            run_crawl(&config, &catalog)?;
        }
        Commands::Purge { catalog, all } => {
            run_purge(&config, catalog.as_deref(), all)?;
        }
        Commands::DumpChunks { file, target_size } => {
            run_dump_chunks(&file, target_size)?;
        }
        Commands::Query { text, limit, catalog } => {
            run_query(&config, &text, limit, catalog.as_deref())?;
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

/// Run crawl (incremental sync)
fn run_crawl(config: &Config, catalog_name: &str) -> anyhow::Result<()> {
    let total_start = std::time::Instant::now();
    println!("🔍 Starting crawl...");
    println!("Catalog: {}", catalog_name);
    
    // Get catalog config
    let catalog_config = config.catalogs.get(catalog_name)
        .ok_or_else(|| anyhow::anyhow!("Catalog '{}' not found in config", catalog_name))?;
    
    let directory = &catalog_config.path;
    
    println!("Directory: {}", directory);
    println!("Type: {}", catalog_config.r#type);
    println!("Collection: {}", config.qdrant.collection);
    println!();

    // Initialize parallel embedder
    println!("⚙️  Loading embedding model...");
    let embedder = ParallelEmbedder::new()?;
    println!();

    let uploader = QdrantUploader::new(&config.qdrant.collection, config.qdrant.url.as_deref())?;

    // Get existing files from DB for this catalog
    println!("📂 Checking existing index...");
    let existing_files = uploader.get_catalog_files(catalog_name)?;
    println!("Found {} files already indexed", existing_files.len());
    println!();

    // Scan directory
    println!("📂 Scanning directory...");
    let mut files_to_process: Vec<String> = Vec::new();

    for entry in walkdir::WalkDir::new(directory)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path().to_string_lossy().to_string();
        
        if !should_skip_path(&path) && is_text_file(&path) {
            files_to_process.push(path);
        }
    }

    let total_files = files_to_process.len();
    println!("Found {} files in directory", total_files);
    println!();

    // Categorize files
    let mut new_count = 0;
    let mut changed_count = 0;
    let mut unchanged_count = 0;
    let mut orphaned_count = 0;
    
    // Find orphaned files (in DB but not on disk)
    let files_set: std::collections::HashSet<String> = files_to_process.iter().cloned().collect();
    for (file_path, _) in existing_files.iter() {
        if !files_set.contains(file_path) {
            orphaned_count += 1;
        }
    }

    // Phase 1: Collect all chunks (sequential - file I/O bound)
    println!("📦 Phase 1: Chunking files...");
    let mut all_chunks: Vec<engine::Chunk> = Vec::new();
    let mut chunks_by_type: HashMap<String, usize> = HashMap::new();
    let mut files_deleted = 0;
    
    for (idx, file_path) in files_to_process.iter().enumerate() {
        // Progress indicator
        print!("\r  Chunking file {}/{} ({:.0}%)   ", 
            idx + 1, total_files, 
            ((idx + 1) as f64 / total_files as f64) * 100.0);
        std::io::Write::flush(&mut std::io::stdout())?;

        // Read file and compute hash
        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("\n  ⚠️  Failed to read {}: {}", file_path, e);
                continue;
            }
        };
        
        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let current_hash = format!("sha256:{:x}", hasher.finalize());

        // Check if file changed
        if let Some(existing_hash) = existing_files.get(file_path) {
            if existing_hash == &current_hash {
                unchanged_count += 1;
                continue; // Skip unchanged file
            }
            
            // File changed - delete old chunks
            uploader.delete_file(file_path, catalog_name)?;
            files_deleted += 1;
            changed_count += 1;
        } else {
            new_count += 1;
        }

        // Chunk the file
        let repo_root = &catalog_config.path;
        let package_name_or_folder = if catalog_config.r#type == "monorepo" {
            engine::package_lookup::find_package_name(file_path, repo_root)
        } else {
            // For folder type, use the folder name
            std::path::Path::new(file_path)
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or(catalog_name)
                .to_string()
        };
        
        match chunk_file(file_path, catalog_name, &package_name_or_folder, 6000) {
            Ok(chunks) => {
                for chunk in &chunks {
                    *chunks_by_type.entry(chunk.chunk_type.clone()).or_insert(0) += 1;
                }
                all_chunks.extend(chunks);
            }
            Err(e) => {
                eprintln!("\n  ⚠️  Failed to chunk file {}: {}", file_path, e);
            }
        }
    }
    
    let total_chunks = all_chunks.len();
    println!("\n  Found {} chunks to embed", total_chunks);
    println!();

    // Phase 2: Parallel embedding with time-based checkpoints
    println!("⚡ Phase 2: Embedding {} chunks with {} parallel sessions...", 
        total_chunks, embedder.num_workers());
    println!("  (Checkpoints every 60s - safe to CTRL+C)");
    let embed_start = std::time::Instant::now();
    
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use crossbeam_channel::{unbounded, Sender, Receiver};
    
    // Channels for streaming embeddings to uploader
    let (embed_tx, embed_rx): (Sender<(engine::Chunk, Vec<f32>)>, Receiver<(engine::Chunk, Vec<f32>)>) = unbounded();
    
    let processed = Arc::new(AtomicUsize::new(0));
    let stop_flag = Arc::new(AtomicBool::new(false));
    
    // Track last upload time
    let last_upload_time = Arc::new(Mutex::new(std::time::Instant::now()));
    
    // Progress reporter thread - prints every 30 seconds
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
                
                eprintln!("[{}] Embedded {}/{} ({:.0}%) - {:.1} chunks/sec - ETA: {}", 
                    chrono_timestamp(),
                    current, total_chunks_for_thread, 
                    (current as f64 / total_chunks_for_thread as f64) * 100.0,
                    rate, eta);
                
                *last = std::time::Instant::now();
            }
        }
    });
    
    // Wrap uploader in Arc<Mutex> for sharing across threads
    let uploader = Arc::new(Mutex::new(uploader));
    
    // Uploader thread - uploads accumulated embeddings every 60 seconds
    let stop_uploader = Arc::clone(&stop_flag);
    let last_upload_time_clone = Arc::clone(&last_upload_time);
    let uploader_clone = Arc::clone(&uploader);
    
    let uploader_thread = std::thread::spawn(move || {
        let mut accumulated: Vec<(engine::Chunk, Vec<f32>)> = Vec::new();
        
        loop {
            // Check if we should upload (60s elapsed or stopped)
            let should_upload = {
                let mut last = last_upload_time_clone.lock().unwrap();
                if last.elapsed() >= std::time::Duration::from_secs(60) {
                    *last = std::time::Instant::now();
                    true
                } else {
                    false
                }
            };
            
            // Collect all available embeddings
            while let Ok(embedded) = embed_rx.try_recv() {
                accumulated.push(embedded);
            }
            
            if should_upload && !accumulated.is_empty() {
                let count = accumulated.len();
                eprintln!("[{}] Uploading checkpoint ({} chunks)...", chrono_timestamp(), count);
                let uploader_guard = uploader_clone.lock().unwrap();
                if let Err(e) = uploader_guard.upload_batch(&accumulated) {
                    eprintln!("[{}] ⚠️ Upload failed: {}", chrono_timestamp(), e);
                }
                drop(uploader_guard);
                eprintln!("[{}] Checkpoint saved", chrono_timestamp());
                accumulated.clear();
            }
            
            // Check if done
            if stop_uploader.load(Ordering::Relaxed) {
                // Drain remaining
                while let Ok(embedded) = embed_rx.try_recv() {
                    accumulated.push(embedded);
                }
                
                // Final upload
                if !accumulated.is_empty() {
                    eprintln!("[{}] Uploading final batch ({} chunks)...", chrono_timestamp(), accumulated.len());
                    let uploader_guard = uploader_clone.lock().unwrap();
                    if let Err(e) = uploader_guard.upload_batch(&accumulated) {
                        eprintln!("[{}] ⚠️ Final upload failed: {}", chrono_timestamp(), e);
                    }
                }
                break;
            }
            
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });
    
    // Process all chunks in parallel, streaming results to uploader
    let embed_tx_clone = embed_tx.clone();
    let processed_embed = Arc::clone(&processed);
    
    all_chunks
        .into_par_iter()
        .enumerate()
        .try_for_each(|(i, chunk)| -> anyhow::Result<()> {
            let embedding = embedder.encode(&chunk.text, i)?;
            
            // Update counter
            processed_embed.fetch_add(1, Ordering::Relaxed);
            
            // Send to uploader
            embed_tx_clone.send((chunk, embedding))?;
            
            Ok(())
        })?;
    
    // Signal threads to stop
    stop_flag.store(true, Ordering::Relaxed);
    
    // Wait for threads
    drop(embed_tx); // Close channel
    let _ = progress_thread.join();
    let _ = uploader_thread.join();
    
    let embed_elapsed = embed_start.elapsed();
    let total_uploaded = processed.load(Ordering::Relaxed);
    let embed_rate = if embed_elapsed.as_secs() > 0 {
        total_uploaded as f64 / embed_elapsed.as_secs_f64()
    } else {
        total_uploaded as f64
    };
    println!();
    println!("  ✅ Embedded & uploaded {} chunks in {}", total_uploaded, format_duration(embed_elapsed.as_secs_f64()));
    println!("  📊 Embedding rate: {:.1} chunks/sec", embed_rate);
    println!();
    
    // Phase 3: Cleanup orphaned files
    println!("🗑️  Cleaning up orphaned files...");
    {
        let uploader_guard = uploader.lock().unwrap();
        for (file_path, _) in existing_files.iter() {
            if !files_set.contains(file_path) {
                uploader_guard.delete_file(file_path, catalog_name)?;
                files_deleted += 1;
            }
        }
    }

    println!();
    println!();
    let total_elapsed = total_start.elapsed();
    
    println!("✅ Crawl complete!");
    println!();
    println!("📊 Summary:");
    println!("  Total time: {:?}", total_elapsed);
    println!("  New files indexed: {}", new_count);
    println!("  Changed files re-indexed: {}", changed_count);
    println!("  Unchanged files skipped: {}", unchanged_count);
    println!("  Orphaned files deleted: {}", orphaned_count);
    println!();
    println!("Total chunks indexed: {}", total_chunks);
    println!("Overall rate: {:.1} chunks/sec", total_chunks as f64 / total_elapsed.as_secs_f64().max(0.001));
    println!("Files deleted from DB: {}", files_deleted);
    println!();

    Ok(())
}

/// Run query
fn run_query(config: &Config, text: &str, limit: usize, catalog: Option<&str>) -> anyhow::Result<()> {
    println!("🔍 Querying Qdrant...");
    println!("Query: \"{}\"", text);
    if let Some(cat) = catalog {
        println!("Catalog filter: {}", cat);
    }
    println!("Limit: {}", limit);
    println!();

    // Generate embedding for query using ParallelEmbedder
    println!("⚙️  Generating embedding for query...");
    let embedder = ParallelEmbedder::new()?;
    let embedding = embedder.encode(text, 0)?;
    println!("✅ Embedding generated");
    println!();

    // Query Qdrant
    println!("🔎 Searching...");
    let uploader = QdrantUploader::new(&config.qdrant.collection, config.qdrant.url.as_deref())?;
    let results = uploader.query(&embedding, limit, catalog)?;

    // Display results
    println!();
    println!("Found {} results:", results.len());
    println!();

    for (idx, result) in results.iter().enumerate() {
        println!("{}. #{:08x}  Score: {:.3}", idx + 1, ((&result.id) >> 32) as u32, result.score);
        println!("   Catalog: {}", result.payload.catalog);
        println!("   Source: {}", result.payload.source_uri);
        println!("   Lines: {}-{}", result.payload.start_line, result.payload.end_line);
        println!("   Type: {}", result.payload.chunk_type);
        
        if let Some(ref symbol) = result.payload.symbol_name {
            println!("   Symbol: {}", symbol);
        }
        
        println!("   Preview:");
        let preview: Vec<&str> = result.payload.text.lines().take(3).collect();
        for line in preview {
            println!("     {}", line);
        }
        
        if result.payload.text.lines().count() > 3 {
            println!("     ...");
        }
        
        println!();
    }

    Ok(())
}


/// Run purge command (delete all chunks from a catalog or entire collection)
fn run_purge(config: &Config, catalog: Option<&str>, all: bool) -> anyhow::Result<()> {
    let uploader = QdrantUploader::new(&config.qdrant.collection, config.qdrant.url.as_deref())?;

    if all {
        println!("🗑️  Purging entire collection: {}", config.qdrant.collection);
        println!("This will delete ALL data from the collection!");
        
        // Delete all points with empty filter
        let endpoint = format!(
            "{}/collections/{}/points/delete",
            config.qdrant.url.as_deref().unwrap_or("http://localhost:6333"),
            config.qdrant.collection
        );
        
        let empty_filter = serde_json::json!({"filter": {}});
        
        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            .json(&empty_filter)
            .send()?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!("Failed to purge collection: HTTP {}", response.status()));
        }
        
        println!("✅ Collection purged successfully");
    } else if let Some(catalog_name) = catalog {
        println!("🗑️  Purging catalog: {}", catalog_name);
        
        let operation_id = uploader.delete_catalog(catalog_name)?;
        println!("✅ Catalog purged successfully (operation ID: {})", operation_id);
    } else {
        return Err(anyhow::anyhow!("Must specify either --catalog <name> or --all"));
    }

    Ok(())
}

/// Check if a file is a text file we want to index
fn is_text_file(path: &str) -> bool {
    let extensions = [
        "ts", "tsx", "js", "jsx",           // TypeScript/JavaScript
        "md", "mdx",                        // Markdown
        "json",                             // JSON
        "yaml", "yml",                      // YAML
        "txt", "rst", "mdn",                // Text docs
        "toml", "ini", "conf",              // Config files
    ];

    let path_lower = path.to_lowercase();
    extensions.iter().any(|ext| path_lower.ends_with(&format!(".{}", ext)))
}

/// Run chunking diagnostics on a TypeScript file
fn run_dump_chunks(file: &PathBuf, target_size: usize) -> anyhow::Result<()> {
    println!("📦 Chunks for: {}", file.display());
    println!();
    
    // Read file
    let source = std::fs::read_to_string(file)
        .map_err(|e| anyhow::anyhow!("Failed to read file: {}", e))?;
    
    // Determine file name and package name
    let file_name = file.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown.ts");
    
    // Find package name by walking upward to find nearest package.json
    let file_path = file.to_string_lossy().to_string();
    let package_name = engine::package_lookup::find_package_name(&file_path, "");
    
    // Create config
    let config = PartitionConfig {
        target_size,
        max_breadcrumb_depth: 4,
        file_name: file_name.to_string(),
        package_name: package_name.clone(),
    };
    
    // Partition
    let chunks = partition_typescript(&source, &config, &file_path, &package_name);
    
    // Display results
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
        println!("Size: {} chars (text: {}, breadcrumb: {})", 
            total_size, text_size, chunk.breadcrumb.len());
        if text_size > target_size {
            println!("⚠️  OVERSIZED (target: {}, actual: {})", target_size, text_size);
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
    println!("Average size: {:.0} chars", total_chars as f64 / chunks.len() as f64);
    println!("Oversized chunks (>{}): {}", target_size, oversized);
    println!("Small chunks (<200): {}", undersized);
    
    Ok(())
}
