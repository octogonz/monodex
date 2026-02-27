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
        Commands::Index { directory, collection, chunk_lines, purge } => {
            run_index(&directory, &collection, chunk_lines, purge)?;
        }
        Commands::Query { text, collection, limit } => {
            run_query(&text, &collection, limit)?;
        }
    }

    Ok(())
}

/// Run indexing
fn run_index(directory: &str, collection: &str, chunk_lines: usize, purge: bool) -> anyhow::Result<()> {
    println!("🔍 Starting indexing...");
    println!("Directory: {}", directory);
    println!("Collection: {}", collection);
    println!("Chunk lines: {}", chunk_lines);
    println!();

    // Initialize components
    println!("⚙️  Loading embedding model...");
    let generator = EmbeddingGenerator::new(MODEL_ID)?;
    println!("✅ Model loaded");
    println!();

    let uploader = QdrantUploader::new(collection, None)?;

    // Purge collection if requested
    if purge {
        println!("🗑️  Purging collection...");
        uploader.purge()?;
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

    println!("✅ Found {} files to index", files_to_index.len());
    println!();

    // Process files
    let mut total_chunks = 0;
    let mut batch: Vec<(engine::Chunk, Vec<f32>)> = Vec::new();

    for (idx, file_path) in files_to_index.iter().enumerate() {
        if idx % 100 == 0 {
            println!("  Processing file {} / {}", idx, files_to_index.len());
        }

        // Chunk the file
        match chunk_file(file_path, chunk_lines) {
            Ok(chunks) => {
                for chunk in chunks {
                    // Generate embedding
                    match generator.encode(&chunk.text) {
                        Ok(embedding) => {
                            batch.push((chunk, embedding));
                            total_chunks += 1;

                            // Upload batch if full
                            if batch.len() >= BATCH_SIZE {
                                uploader.upload_batch(&batch)?;
                                print!("\r  Uploaded {} chunks...   ", total_chunks);
                                std::io::stdout().flush()?;
                                batch.clear();
                            }
                        }
                        Err(e) => {
                            eprintln!("\n  ⚠️  Failed to embed chunk in {}: {}", file_path, e);
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
    if !batch.is_empty() {
        uploader.upload_batch(&batch)?;
        print!("\r  Uploaded {} chunks...   ", total_chunks);
        std::io::stdout().flush()?;
    }

    println!();
    println!();
    println!("✅ Indexing complete!");
    println!("Total chunks indexed: {}", total_chunks);
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
