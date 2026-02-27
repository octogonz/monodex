//! rush-qdrant: Semantic search indexer for Rush monorepos
//! 
//! Uses Qdrant vector database with BAAI/bge-small-en-v1.5 embeddings
//! Intelligently chunks code and documentation for high-quality semantic search

mod engine;

use clap::{Parser, Subcommand};
use engine::config::{should_skip_path, get_chunk_strategy};

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

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Index { directory, collection, chunk_lines } => {
            run_index(&directory, &collection, chunk_lines)?;
        }
        Commands::Query { text, collection, limit } => {
            run_query(&text, &collection, limit)?;
        }
    }

    Ok(())
}

/// Run indexing (simplified version - will improve later)
fn run_index(directory: &str, collection: &str, chunk_lines: usize) -> anyhow::Result<()> {
    println!("🔍 Starting indexing...");
    println!("Directory: {}", directory);
    println!("Collection: {}", collection);
    println!("Chunk lines: {}", chunk_lines);
    println!();
    
    // For now, just list files (placeholder for full implementation)
    let mut file_count = 0;
    
    for entry in walkdir::WalkDir::new(directory)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().is_file()
                && !should_skip_path(&e.path().to_string_lossy())
        })
    {
        let path = entry.path().to_string_lossy();
        
        if path.ends_with(".ts") || path.ends_with(".tsx") || path.ends_with(".js") || path.ends_with(".md") || path.ends_with(".json") {
            file_count += 1;
            if file_count % 100 == 0 {
                println!("  Scanned {} files...", file_count);
            }
        }
    }
    
    println!();
    println!("✅ Placeholder: Indexing logic not yet implemented");
    println!("Found {} relevant files", file_count);
    println!("Next: Implement chunker, embedder, and uploader modules");
    println!();
    println!("TODO: Tree-sitter for TypeScript/JavaScript chunking");
    println!("TODO: Embedding generation with Candle");
    println!("TODO: Qdrant batch uploads");
    
    Ok(())
}

/// Run query (placeholder - uses test-query.sh for now)
fn run_query(text: &str, collection: &str, limit: usize) -> anyhow::Result<()> {
    println!("🔍 Querying Qdrant...");
    println!("Query: {}", text);
    println!("Collection: {}", collection);
    println!("Limit: {}", limit);
    println!();
    
    println!("⚠️  Placeholder: Using test-query.sh script");
    println!("Next: Implement direct Rust query with embedding generation");
    println!("Next: Qdrant HTTP client in Rust");
    
    Ok(())
}
