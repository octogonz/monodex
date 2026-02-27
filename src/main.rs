//! rush-qdrant: Semantic search indexer for Rush monorepos
//! 
//! Uses Qdrant vector database with BAAI/bge-small-en-v1.5 embeddings
//! Intelligently chunks code and documentation for high-quality semantic search

mod engine;

use clap::{Parser, Subcommand};
use std::io::Write;
use engine::{
    config::should_skip_path,
    chunker::chunk_file,
    embedder::EmbeddingGenerator,
    uploader::QdrantUploader,
};

/// CLI structure
#[derive(Parser)]
#[command(name = "rush-qdrant", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// Available commands
#[derive(Subcommand)]
enum Commands {
    /// Index repository code and docs into Qdrant
    Index {
        /// Directory to index
        #[arg(long)]
        directory: String,
        
        /// Qdrant collection name
        #[arg(long, default_value = "rushstack-ai")]
        collection: String,
        
        /// Chunk size in lines
        #[arg(long, default_value = "100")]
        chunk_lines: usize,
        
        /// Purge collection before indexing (delete all existing points)
        #[arg(long)]
        purge: bool,
        
        /// Dry-run: count chunks and report without embedding or uploading
        #[arg(long)]
        dry_run: bool,
    },
    
    /// Query the semantic search database
    Query {
        /// Search query text
        #[arg(long)]
        text: String,
        
        /// Qdrant collection name
        #[arg(long, default_value = "rushstack-ai")]
        collection: String,
        
        /// Number of results
        #[arg(long, default_value = "5")]
        limit: usize,
    },
}

const BATCH_SIZE: usize = 32;
const MODEL_ID: &str = "BAAI/bge-small-en-v1.5";

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Index { directory, collection, chunk_lines, purge, dry_run } => {
            run_index(&directory, &collection, chunk_lines, purge, dry_run)?;
        }
        Commands::Query { text, collection, limit } => {
            run_query(&text, &collection, limit)?;
        }
    }

    Ok(())
}

/// Run indexing
fn run_index(directory: &str, collection: &str, chunk_lines: usize, purge: bool, dry_run: bool) -> anyhow::Result<()> {
    println!("🔍 Starting indexing...");
    println!("Directory: {}", directory);
    println!("Collection: {}", collection);
    println!("Chunk lines: {}", chunk_lines);
    if dry_run {
        println!("Mode: DRY-RUN (no changes will be made)");
    }
    println!();

    // Initialize components
    let generator = if !dry_run {
        println!("⚙️  Loading embedding model...");
        let g = EmbeddingGenerator::new(MODEL_ID)?;
        println!("✅ Model loaded");
        println!();
        Some(g)
    } else {
        None
    };

    let uploader = if !dry_run {
        Some(QdrantUploader::new(collection, None)?)
    } else {
        None
    };

    // Purge collection if requested
    if purge && !dry_run {
        println!("🗑️  Purging collection...");
        if let Some(ref uploader) = uploader {
            uploader.purge()?;
        }
        println!("✅ Collection purged");
        println!();
    }

    // Collect files
    println!("📂 Scanning directory...");
    let mut files_to_index: Vec<String> = Vec::new();

    for entry in walkdir::WalkDir::new(directory)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path().to_string_lossy().to_string();
        
        if !should_skip_path(&path) && is_text_file(&path) {
            files_to_index.push(path);
        }
    }

    let total_files = files_to_index.len();
    println!("✅ Found {} files to index", total_files);
    println!();

    // Process files
    let mut total_chunks = 0;
    let mut batch: Vec<(engine::Chunk, Vec<f32>)> = Vec::new();
    let mut chunks_by_type: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for (idx, file_path) in files_to_index.iter().enumerate() {
        // Progress indicator
        print!("\r  Processing file {}/{} ({:.0}%)   ", 
            idx + 1, total_files, 
            ((idx + 1) as f64 / total_files as f64) * 100.0);
        std::io::Write::flush(&mut std::io::stdout())?;

        // Chunk the file
        match chunk_file(file_path, chunk_lines) {
            Ok(chunks) => {
                for chunk in chunks {
                    // Track chunk types for reporting
                    *chunks_by_type.entry(chunk.chunk_type.clone()).or_insert(0) += 1;
                    
                    if dry_run {
                        // Just count, don't embed
                        total_chunks += 1;
                    } else {
                        // Generate embedding
                        if let Some(ref generator) = generator {
                            match generator.encode(&chunk.text) {
                                Ok(embedding) => {
                                    batch.push((chunk, embedding));
                                    total_chunks += 1;

                                    // Upload batch if full
                                    if batch.len() >= BATCH_SIZE {
                                        if let Some(ref uploader) = uploader {
                                            uploader.upload_batch(&batch)?;
                                        }
                                        print!("\r  Uploaded {} chunks...   ", total_chunks);
                                        std::io::Write::flush(&mut std::io::stdout())?;
                                        batch.clear();
                                    }
                                }
                                Err(e) => {
                                    eprintln!("\n  ⚠️  Failed to embed chunk in {}: {}", file_path, e);
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("\n  ⚠️  Failed to chunk file {}: {}", file_path, e);
            }
        }
    }

    // Upload remaining batch
    if !dry_run && !batch.is_empty() {
        if let Some(ref uploader) = uploader {
            uploader.upload_batch(&batch)?;
        }
        print!("\r  Uploaded {} chunks...   ", total_chunks);
        std::io::Write::flush(&mut std::io::stdout())?;
    }

    println!();
    println!();
    
    if dry_run {
        println!("📋 DRY-RUN REPORT");
        println!("================");
        println!("Files scanned: {}", total_files);
        println!("Total chunks that would be created: {}", total_chunks);
        println!();
        println!("Chunks by type:");
        for (chunk_type, count) in chunks_by_type.iter() {
            println!("  {}: {}", chunk_type, count);
        }
        println!();
        
        // Estimate time
        let estimated_seconds = total_chunks as f64 * 1.5; // ~1.5s per chunk
        let estimated_hours = (estimated_seconds / 3600.0).floor() as i32;
        let estimated_minutes = ((estimated_seconds % 3600.0) / 60.0).floor() as i32;
        println!("Estimated indexing time: {}h {}m", estimated_hours, estimated_minutes);
    } else {
        println!("✅ Indexing complete!");
        println!("Total chunks indexed: {}", total_chunks);
    }
    println!();

    Ok(())
}

/// Run query
fn run_query(text: &str, collection: &str, limit: usize) -> anyhow::Result<()> {
    println!("🔍 Querying Qdrant...");
    println!("Query: \"{}\"", text);
    println!("Collection: {}", collection);
    println!("Limit: {}", limit);
    println!();

    // Generate embedding for query
    println!("⚙️  Generating embedding for query...");
    let generator = EmbeddingGenerator::new(MODEL_ID)?;
    let embedding = generator.encode(text)?;
    println!("✅ Embedding generated");
    println!();

    // Query Qdrant
    println!("🔎 Searching...");
    let uploader = QdrantUploader::new(collection, None)?;
    let results = uploader.query(&embedding, limit)?;

    // Display results
    println!();
    println!("Found {} results:", results.len());
    println!();

    for (idx, result) in results.iter().enumerate() {
        println!("{}. Score: {:.3}", idx + 1, result.score);
        println!("   File: {}", result.payload.file);
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
