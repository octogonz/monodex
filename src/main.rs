//! monodex: Semantic search indexer for Rush monorepos
//!
//! Uses Qdrant vector database with jina-embeddings-v2-base-code embeddings
//! Intelligently chunks code and documentation for high-quality semantic search

use clap::Parser;
use monodex::app::{Cli, Commands};
use monodex::app::{
    Config, format_duration, load_config, resolve_label_context, run_use,
    run_embed_upload_pipeline,
};
use monodex::engine::{
    chunker::{ChunkContext, chunk_content},
    crawl_config::load_compiled_crawl_config,
    git_ops::{
        build_package_index_for_commit, build_package_index_for_working_dir, enumerate_commit_tree,
        enumerate_working_directory, read_blob_content, read_working_file_content,
        resolve_commit_oid,
    },
    identifier::LabelId,
    uploader::{LabelMetadata, QdrantUploader},
};
use std::collections::HashSet;
use std::path::PathBuf;

const DEFAULT_CONFIG_PATH: &str = "~/.config/monodex/config.json";

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Load config
    let config_path = cli
        .config
        .unwrap_or_else(|| PathBuf::from(shellexpand::tilde(DEFAULT_CONFIG_PATH).as_ref()));
    let config = load_config(&config_path)?;

    match cli.command {
        Commands::Use { catalog, label } => {
            run_use(catalog.as_deref(), label, &config)?;
        }
        Commands::Crawl {
            catalog,
            label,
            source,
            incremental_warnings,
        } => {
            // Resolve label context from explicit flags or default context
            let (_label_id, catalog_name, label) =
                resolve_label_context(Some(&label), catalog.as_deref())?;

            if source.working_dir {
                run_crawl_working_dir(
                    &config,
                    &catalog_name,
                    &label,
                    incremental_warnings,
                    cli.debug,
                )?;
            } else {
                // Safe to unwrap: clap ArgGroup ensures one of commit/working_dir is set
                run_crawl_label(
                    &config,
                    &catalog_name,
                    &label,
                    source.commit.as_ref().unwrap(),
                    incremental_warnings,
                    cli.debug,
                )?;
            }
        }
        Commands::Purge { catalog, all } => {
            monodex::app::commands::run_purge(&config, catalog.as_deref(), all, cli.debug)?;
        }
        Commands::DumpChunks {
            file,
            target_size,
            visualize,
            with_fallback,
            debug,
        } => {
            monodex::app::commands::run_dump_chunks(
                &file,
                target_size,
                visualize,
                with_fallback,
                debug,
            )?;
        }
        Commands::Search {
            text,
            limit,
            label,
            catalog,
        } => {
            monodex::app::commands::run_search(
                &config,
                &text,
                limit,
                label.as_deref(),
                catalog.as_deref(),
                cli.debug,
            )?;
        }
        Commands::View {
            id,
            label,
            catalog,
            full_paths,
            chunks_only,
        } => {
            monodex::app::commands::run_view(
                &config,
                &id,
                label.as_deref(),
                catalog.as_deref(),
                full_paths,
                chunks_only,
                cli.debug,
            )?;
        }
        Commands::AuditChunks { count, dir } => {
            monodex::app::commands::run_audit_chunks(count, dir)?;
        }
    }

    Ok(())
}

fn run_crawl_label(
    config: &Config,
    catalog_name: &str,
    label: &str,
    commit: &str,
    _incremental_warnings: bool,
    debug: bool,
) -> anyhow::Result<()> {
    use monodex::engine::util::{CHUNKER_ID, EMBEDDER_ID, compute_file_id};

    let total_start = std::time::Instant::now();
    println!("🔍 Starting label-aware crawl...");
    println!("Catalog: {}", catalog_name);
    println!("Label: {}", label);

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

    // Compute label_id (internal storage form)
    let label_id = LabelId::new(catalog_name, label).map_err(|e| anyhow::anyhow!("{}", e))?;

    // B.1: Load repo-specific crawl configuration
    let crawl_config = load_compiled_crawl_config(Some(repo_path))?;
    println!("Loaded crawl configuration for repository");

    // Initialize uploader
    let uploader = QdrantUploader::new(
        &config.qdrant.collection,
        config.qdrant.url.as_deref(),
        debug,
        config.qdrant.get_max_upload_bytes(),
    )?;

    // Step 1: Resolve commit to full SHA and write in-progress metadata
    println!("📦 Resolving commit...");
    let commit_oid = resolve_commit_oid(repo_path, commit)?;
    println!("Resolved {} to {}", commit, &commit_oid[..12]);

    // Write in-progress metadata before any work begins
    let in_progress_metadata = LabelMetadata {
        source_type: "label-metadata".to_string(),
        catalog: catalog_name.to_string(),
        label_id: label_id.to_string(),
        label: label.to_string(),
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
    let mut existing_files_needing_label: HashSet<String> = HashSet::new(); // Files that exist but don't have this label
    let mut existing_files_already_labeled: HashSet<String> = HashSet::new(); // Files that already have this label
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
            Ok(Some(sync_info)) => {
                // File already indexed - check if it already has this label
                if sync_info.active_label_ids.contains(&label_id.to_string()) {
                    // Already has the label - no action needed, but mark as touched for cleanup
                    existing_files_already_labeled.insert(file_id);
                } else {
                    // Needs label added
                    existing_files_needing_label.insert(file_id);
                }
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
    if !existing_files_already_labeled.is_empty() {
        println!(
            "  Existing files already labeled: {} (skipping)",
            existing_files_already_labeled.len()
        );
    }
    println!();

    // Step 5: Add label to existing files that need it
    // Track files that successfully got the label added
    // Also track failures for A.1 - existing file label-add failures must count toward crawl failure
    let mut label_add_success_files: HashSet<String> = HashSet::new();
    let mut existing_file_label_add_failures: Vec<String> = Vec::new();
    if !existing_files_needing_label.is_empty() {
        println!(
            "🏷️  Adding label to {} existing files...",
            existing_files_needing_label.len()
        );
        for file_id in &existing_files_needing_label {
            if let Err(e) = uploader.add_label_to_file_chunks(file_id, &label_id) {
                eprintln!("  ❌ Failed to add label to file {}: {}", file_id, e);
                existing_file_label_add_failures.push(format!("{}: {}", file_id, e));
            } else {
                // Only track as successfully added after the call succeeds (A.3)
                label_add_success_files.insert(file_id.clone());
            }
        }
        println!("  Done.");
        if !existing_file_label_add_failures.is_empty() {
            println!(
                "  ⚠️  Failed to add label to {} existing files",
                existing_file_label_add_failures.len()
            );
        }
        println!();
    }
    // Combine successfully labeled files with already-labeled files for cleanup logic
    let existing_files: HashSet<String> = label_add_success_files
        .union(&existing_files_already_labeled)
        .cloned()
        .collect();

    // Step 6: Index new files
    let mut all_chunks: Vec<monodex::engine::Chunk> = Vec::new();
    let mut touched_file_ids: HashSet<String> = HashSet::new();

    if !new_files.is_empty() {
        println!("📦 Phase 2: Chunking {} new files...", new_count);

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
                label_id: label_id.to_string(),
                package_name,
                relative_path: relative_path.clone(),
                blob_id: blob_id.clone(),
                source_uri: format!("{}/{}", repo_path.display(), relative_path),
            };

            // Chunk the content - B.1: pass strategy from discovered crawl config
            let strategy = crawl_config.get_strategy(relative_path);
            match chunk_content(&content_str, &ctx, 6000, strategy) {
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
    }

    // Phase 3: Run the shared embed/upload pipeline (handles empty chunks gracefully)
    let (pipeline_file_ids, pipeline_failures) =
        run_embed_upload_pipeline(all_chunks, uploader, &label_id, &config.embedding_model)?;

    // Merge file IDs from pipeline with those tracked during chunking
    touched_file_ids.extend(pipeline_file_ids);

    // A.1: Include existing-file label-add failures in the failure check
    let has_existing_file_failures = !existing_file_label_add_failures.is_empty();
    let had_failures = pipeline_failures.has_failures() || has_existing_file_failures;

    // Step 7: Label reassignment cleanup (A.1: ONLY after fully successful crawl)
    // Remove label from chunks that were NOT touched in this crawl
    // A.2: Track cleanup failure separately
    let mut cleanup_failed = false;
    if had_failures {
        println!("🧹 Phase 4: SKIPPING label reassignment cleanup (crawl had failures)");
        println!("  This is intentional - cleanup should only run after successful crawls.");
        println!("  Run the crawl again to complete indexing and trigger cleanup.");
    } else {
        println!("🧹 Phase 4: Label reassignment cleanup...");
        let all_touched: HashSet<String> =
            existing_files.union(&touched_file_ids).cloned().collect();

        // Create a new uploader for cleanup (the previous one was moved into the uploader thread)
        let cleanup_uploader = QdrantUploader::new(
            &config.qdrant.collection,
            config.qdrant.url.as_deref(),
            debug,
            config.qdrant.get_max_upload_bytes(),
        )?;
        match cleanup_uploader.remove_label_from_chunks(&label_id, &all_touched) {
            Ok(processed) => {
                println!("  Processed {} chunks for label cleanup", processed);
            }
            Err(e) => {
                // A.2: Cleanup failure should block crawl_complete
                eprintln!("  ❌ Label cleanup failed: {}", e);
                cleanup_failed = true;
            }
        }
    }
    println!();

    // Step 8: Update label metadata (A.1: set crawl_complete=false if failures occurred)
    // A.2: Also set crawl_complete=false if cleanup failed
    println!("📝 Updating label metadata...");
    let crawl_complete = !had_failures && !cleanup_failed;
    let metadata = LabelMetadata {
        source_type: "label-metadata".to_string(),
        catalog: catalog_name.to_string(),
        label_id: label_id.to_string(),
        label: label.to_string(),
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
    let uploader = QdrantUploader::new(
        &config.qdrant.collection,
        config.qdrant.url.as_deref(),
        debug,
        config.qdrant.get_max_upload_bytes(),
    )?;
    uploader.upsert_label_metadata(&metadata)?;
    if crawl_complete {
        println!("  Label metadata saved.");
    } else {
        println!("  Label metadata saved (crawl_complete=false due to failures).");
    }
    println!();

    // Summary
    let total_elapsed = total_start.elapsed();
    if had_failures || cleanup_failed {
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
        let total_failures = pipeline_failures.total() + existing_file_label_add_failures.len();
        println!("  Total failures: {}", total_failures);
        if has_existing_file_failures {
            println!(
                "  - Existing file label-add failures: {}",
                existing_file_label_add_failures.len()
            );
        }
        if cleanup_failed {
            println!("  - Label cleanup failed (crawl not marked complete)");
        }
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
    label: &str,
    _incremental_warnings: bool,
    debug: bool,
) -> anyhow::Result<()> {
    use monodex::engine::util::{CHUNKER_ID, EMBEDDER_ID, compute_file_id};

    let total_start = std::time::Instant::now();
    println!("🔍 Starting working directory crawl...");
    println!("Catalog: {}", catalog_name);
    println!("Label: {}", label);

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

    // Compute label_id (internal storage form)
    let label_id = LabelId::new(catalog_name, label).map_err(|e| anyhow::anyhow!("{}", e))?;

    // B.1: Load repo-specific crawl configuration
    let crawl_config = load_compiled_crawl_config(Some(repo_path))?;
    println!("Loaded crawl configuration for repository");

    // Initialize uploader
    let uploader = QdrantUploader::new(
        &config.qdrant.collection,
        config.qdrant.url.as_deref(),
        debug,
        config.qdrant.get_max_upload_bytes(),
    )?;

    // Write in-progress metadata
    let in_progress_metadata = LabelMetadata {
        source_type: "label-metadata".to_string(),
        catalog: catalog_name.to_string(),
        label_id: label_id.to_string(),
        label: label.to_string(),
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

    let mut new_files: Vec<(String, String)> = Vec::new(); // (relative_path, blob_id)
    let mut existing_files_needing_label: HashSet<String> = HashSet::new(); // Files that exist but don't have this label
    let mut existing_files_already_labeled: HashSet<String> = HashSet::new(); // Files that already have this label
    let mut new_count = 0;
    let mut existing_count = 0;

    for file_entry in &files_to_process {
        let file_id = compute_file_id(
            EMBEDDER_ID,
            CHUNKER_ID,
            &file_entry.blob_id,
            &file_entry.relative_path,
        );

        match uploader.get_file_sentinel(&file_id) {
            Ok(Some(sync_info)) => {
                // File already indexed - check if it already has this label
                if sync_info.active_label_ids.contains(&label_id.to_string()) {
                    // Already has the label - no action needed, but mark as touched for cleanup
                    existing_files_already_labeled.insert(file_id);
                } else {
                    // Needs label added
                    existing_files_needing_label.insert(file_id);
                }
                existing_count += 1;
            }
            Ok(None) => {
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
    if !existing_files_already_labeled.is_empty() {
        println!(
            "  Existing files already labeled: {} (skipping)",
            existing_files_already_labeled.len()
        );
    }
    println!();

    // Add label to existing files that need it
    // A.1/A.3: Track files that successfully got the label added, and track failures
    let mut label_add_success_files: HashSet<String> = HashSet::new();
    let mut existing_file_label_add_failures: Vec<String> = Vec::new();
    if !existing_files_needing_label.is_empty() {
        println!(
            "🏷️  Adding label to {} existing files...",
            existing_files_needing_label.len()
        );
        for file_id in &existing_files_needing_label {
            if let Err(e) = uploader.add_label_to_file_chunks(file_id, &label_id) {
                eprintln!("  ❌ Failed to add label to file {}: {}", file_id, e);
                existing_file_label_add_failures.push(format!("{}: {}", file_id, e));
            } else {
                label_add_success_files.insert(file_id.clone());
            }
        }
        println!("  Done.");
        if !existing_file_label_add_failures.is_empty() {
            println!(
                "  ⚠️  Failed to add label to {} existing files",
                existing_file_label_add_failures.len()
            );
        }
        println!();
    }
    // Combine successfully labeled files with already-labeled files for cleanup logic
    let existing_files: HashSet<String> = label_add_success_files
        .union(&existing_files_already_labeled)
        .cloned()
        .collect();

    // Step 6: Index new files
    let mut all_chunks: Vec<monodex::engine::Chunk> = Vec::new();
    let mut touched_file_ids: HashSet<String> = HashSet::new();

    if !new_files.is_empty() {
        println!("📦 Phase 2: Chunking {} new files...", new_count);

        for (idx, (relative_path, blob_id)) in new_files.iter().enumerate() {
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

            // Create chunk context
            let ctx = ChunkContext {
                catalog: catalog_name.to_string(),
                label_id: label_id.to_string(),
                package_name,
                relative_path: relative_path.clone(),
                blob_id: blob_id.clone(),
                source_uri: format!("{}/{}", repo_path.display(), relative_path),
            };

            // Chunk the content - B.1: pass strategy from discovered crawl config
            let strategy = crawl_config.get_strategy(relative_path);
            match chunk_content(&content_str, &ctx, 6000, strategy) {
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
    }

    // Phase 3: Run the shared embed/upload pipeline (handles empty chunks gracefully)
    let (pipeline_file_ids, pipeline_failures) =
        run_embed_upload_pipeline(all_chunks, uploader, &label_id, &config.embedding_model)?;

    // Merge file IDs from pipeline with those tracked during chunking
    touched_file_ids.extend(pipeline_file_ids);

    // A.1: Include existing-file label-add failures in the failure check
    let has_existing_file_failures = !existing_file_label_add_failures.is_empty();
    let had_failures = pipeline_failures.has_failures() || has_existing_file_failures;

    // Step 7: Label reassignment cleanup (A.1: ONLY after fully successful crawl)
    // A.2: Track cleanup failure separately
    let mut cleanup_failed = false;
    if had_failures {
        println!("🧹 Phase 4: SKIPPING label reassignment cleanup (crawl had failures)");
        println!("  This is intentional - cleanup should only run after successful crawls.");
        println!("  Run the crawl again to complete indexing and trigger cleanup.");
    } else {
        println!("🧹 Phase 4: Label reassignment cleanup...");
        let all_touched: HashSet<String> =
            existing_files.union(&touched_file_ids).cloned().collect();

        let cleanup_uploader = QdrantUploader::new(
            &config.qdrant.collection,
            config.qdrant.url.as_deref(),
            debug,
            config.qdrant.get_max_upload_bytes(),
        )?;
        match cleanup_uploader.remove_label_from_chunks(&label_id, &all_touched) {
            Ok(processed) => println!("  Processed {} chunks for label cleanup", processed),
            Err(e) => {
                // A.2: Cleanup failure should block crawl_complete
                eprintln!("  ❌ Label cleanup failed: {}", e);
                cleanup_failed = true;
            }
        }
    }
    println!();

    // Update label metadata (A.1: set crawl_complete=false if failures occurred)
    // A.2: Also set crawl_complete=false if cleanup failed
    println!("📝 Updating label metadata...");
    let crawl_complete = !had_failures && !cleanup_failed;
    let metadata = LabelMetadata {
        source_type: "label-metadata".to_string(),
        catalog: catalog_name.to_string(),
        label_id: label_id.to_string(),
        label: label.to_string(),
        commit_oid: "".to_string(),
        source_kind: "working-directory".to_string(),
        crawl_complete,
        updated_at_unix_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    };

    let uploader = QdrantUploader::new(
        &config.qdrant.collection,
        config.qdrant.url.as_deref(),
        debug,
        config.qdrant.get_max_upload_bytes(),
    )?;
    uploader.upsert_label_metadata(&metadata)?;
    if crawl_complete {
        println!("  Label metadata saved.");
    } else {
        println!("  Label metadata saved (crawl_complete=false due to failures).");
    }
    println!();

    let total_elapsed = total_start.elapsed();
    if had_failures || cleanup_failed {
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
        let total_failures = pipeline_failures.total() + existing_file_label_add_failures.len();
        println!("  Total failures: {}", total_failures);
        if has_existing_file_failures {
            println!(
                "  - Existing file label-add failures: {}",
                existing_file_label_add_failures.len()
            );
        }
        if cleanup_failed {
            println!("  - Label cleanup failed (crawl not marked complete)");
        }
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
