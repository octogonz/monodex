//! Chunk table operations for LanceDB storage.
//!
//! Purpose: Provide typed operations on the `chunks` table.
//!
//! Edit here when: Adding/modifying chunk storage operations, vector search logic,
//!   or label membership updates.
//! Do not edit here for: Row types (see rows.rs), label metadata operations (see labels.rs),
//!   database open logic (see database.rs).

use anyhow::{Result, anyhow};
use arrow_array::{
    Array, ArrayRef, BooleanArray, FixedSizeListArray, Float32Array, Int32Array, ListArray,
    RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_buffer::{OffsetBuffer, ScalarBuffer};
use arrow_schema::{DataType, Field, SchemaRef};
use futures::TryStreamExt;
use lancedb::DistanceType;
use lancedb::query::{ExecutableQuery, QueryBase};
use std::sync::Arc;

use crate::engine::schema::VECTOR_DIMENSION;
use crate::engine::storage::{ChunkRow, ScoredChunkRow};

/// Convert an iterator of ChunkRows to a RecordBatch.
///
/// Handles nullable fields and list columns correctly.
fn chunk_rows_to_record_batch<'a>(
    rows: impl IntoIterator<Item = &'a ChunkRow>,
    schema: SchemaRef,
) -> Result<RecordBatch> {
    let rows: Vec<&ChunkRow> = rows.into_iter().collect();
    let n = rows.len();

    let point_id: StringArray = rows.iter().map(|r| Some(r.point_id.as_str())).collect();
    let text: StringArray = rows.iter().map(|r| Some(r.text.as_str())).collect();

    // Vector column (FixedSizeList<Float32, 768>)
    // Note: We don't store vectors in ChunkRow, they're computed during embedding
    // For now, we'll use a placeholder. The actual vector is passed separately.
    // This function is for reading, vectors come from LanceDB directly.
    let vector_field = Field::new("item", DataType::Float32, true);
    let vector_values: Float32Array = std::iter::repeat_n(0.0f32, VECTOR_DIMENSION * n).collect();
    let vector: ArrayRef = Arc::new(FixedSizeListArray::new(
        Arc::new(vector_field),
        VECTOR_DIMENSION as i32,
        Arc::new(vector_values),
        None,
    ));

    let catalog: StringArray = rows.iter().map(|r| Some(r.catalog.as_str())).collect();
    let label_id: StringArray = rows.iter().map(|r| Some(r.label_id.as_str())).collect();

    // active_label_ids: List<Utf8>
    let active_label_ids = build_string_list_array(
        &rows
            .iter()
            .map(|r| r.active_label_ids.as_slice())
            .collect::<Vec<_>>(),
    );

    let embedder_id: StringArray = rows.iter().map(|r| Some(r.embedder_id.as_str())).collect();
    let chunker_id: StringArray = rows.iter().map(|r| Some(r.chunker_id.as_str())).collect();
    let blob_id: StringArray = rows.iter().map(|r| Some(r.blob_id.as_str())).collect();
    let content_hash: StringArray = rows.iter().map(|r| Some(r.content_hash.as_str())).collect();
    let file_id: StringArray = rows.iter().map(|r| Some(r.file_id.as_str())).collect();
    let relative_path: StringArray = rows
        .iter()
        .map(|r| Some(r.relative_path.as_str()))
        .collect();
    let package_name: StringArray = rows.iter().map(|r| Some(r.package_name.as_str())).collect();
    let source_uri: StringArray = rows.iter().map(|r| Some(r.source_uri.as_str())).collect();

    let chunk_ordinal: Int32Array = rows.iter().map(|r| Some(r.chunk_ordinal)).collect();
    let chunk_count: Int32Array = rows.iter().map(|r| Some(r.chunk_count)).collect();
    let start_line: Int32Array = rows.iter().map(|r| Some(r.start_line)).collect();
    let end_line: Int32Array = rows.iter().map(|r| Some(r.end_line)).collect();

    // Nullable string fields
    let symbol_name: StringArray = rows.iter().map(|r| r.symbol_name.as_deref()).collect();
    let chunk_type: StringArray = rows.iter().map(|r| Some(r.chunk_type.as_str())).collect();
    let chunk_kind: StringArray = rows.iter().map(|r| Some(r.chunk_kind.as_str())).collect();
    let breadcrumb: StringArray = rows.iter().map(|r| r.breadcrumb.as_deref()).collect();

    // Nullable int fields
    let split_part_ordinal: Int32Array = rows.iter().map(|r| r.split_part_ordinal).collect();
    let split_part_count: Int32Array = rows.iter().map(|r| r.split_part_count).collect();

    let file_complete: BooleanArray = rows.iter().map(|r| Some(r.file_complete)).collect();

    let columns: Vec<ArrayRef> = vec![
        Arc::new(point_id),
        Arc::new(text),
        vector,
        Arc::new(catalog),
        Arc::new(label_id),
        active_label_ids,
        Arc::new(embedder_id),
        Arc::new(chunker_id),
        Arc::new(blob_id),
        Arc::new(content_hash),
        Arc::new(file_id),
        Arc::new(relative_path),
        Arc::new(package_name),
        Arc::new(source_uri),
        Arc::new(chunk_ordinal),
        Arc::new(chunk_count),
        Arc::new(start_line),
        Arc::new(end_line),
        Arc::new(symbol_name),
        Arc::new(chunk_type),
        Arc::new(chunk_kind),
        Arc::new(breadcrumb),
        Arc::new(split_part_ordinal),
        Arc::new(split_part_count),
        Arc::new(file_complete),
    ];

    RecordBatch::try_new(schema, columns)
        .map_err(|e| anyhow!("Failed to create RecordBatch: {}", e))
}

/// Convert an iterator of ChunkRows with their vectors to a RecordBatch.
///
/// This is the primary function for writing chunks during crawl, where we have
/// both the row data and the computed embedding vectors.
fn chunk_rows_to_record_batch_with_vectors<'a>(
    rows: impl IntoIterator<Item = (&'a ChunkRow, &'a [f32])>,
    schema: SchemaRef,
) -> Result<RecordBatch> {
    let rows: Vec<(&ChunkRow, &[f32])> = rows.into_iter().collect();
    let n = rows.len();

    let point_id: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.point_id.as_str()))
        .collect();
    let text: StringArray = rows.iter().map(|(r, _)| Some(r.text.as_str())).collect();

    // Vector column with actual embedding values
    let vector_field = Field::new("item", DataType::Float32, true);
    let mut all_vector_values: Vec<f32> = Vec::with_capacity(VECTOR_DIMENSION * n);
    for (_, vector) in &rows {
        if vector.len() != VECTOR_DIMENSION {
            return Err(anyhow!(
                "Vector dimension mismatch: expected {}, got {}",
                VECTOR_DIMENSION,
                vector.len()
            ));
        }
        all_vector_values.extend_from_slice(vector);
    }
    let vector_values: Float32Array = all_vector_values.into();
    let vector: ArrayRef = Arc::new(FixedSizeListArray::new(
        Arc::new(vector_field),
        VECTOR_DIMENSION as i32,
        Arc::new(vector_values),
        None,
    ));

    let catalog: StringArray = rows.iter().map(|(r, _)| Some(r.catalog.as_str())).collect();
    let label_id: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.label_id.as_str()))
        .collect();

    // active_label_ids: List<Utf8>
    let active_label_ids = build_string_list_array(
        &rows
            .iter()
            .map(|(r, _)| r.active_label_ids.as_slice())
            .collect::<Vec<_>>(),
    );

    let embedder_id: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.embedder_id.as_str()))
        .collect();
    let chunker_id: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.chunker_id.as_str()))
        .collect();
    let blob_id: StringArray = rows.iter().map(|(r, _)| Some(r.blob_id.as_str())).collect();
    let content_hash: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.content_hash.as_str()))
        .collect();
    let file_id: StringArray = rows.iter().map(|(r, _)| Some(r.file_id.as_str())).collect();
    let relative_path: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.relative_path.as_str()))
        .collect();
    let package_name: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.package_name.as_str()))
        .collect();
    let source_uri: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.source_uri.as_str()))
        .collect();

    let chunk_ordinal: Int32Array = rows.iter().map(|(r, _)| Some(r.chunk_ordinal)).collect();
    let chunk_count: Int32Array = rows.iter().map(|(r, _)| Some(r.chunk_count)).collect();
    let start_line: Int32Array = rows.iter().map(|(r, _)| Some(r.start_line)).collect();
    let end_line: Int32Array = rows.iter().map(|(r, _)| Some(r.end_line)).collect();

    // Nullable string fields
    let symbol_name: StringArray = rows.iter().map(|(r, _)| r.symbol_name.as_deref()).collect();
    let chunk_type: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.chunk_type.as_str()))
        .collect();
    let chunk_kind: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.chunk_kind.as_str()))
        .collect();
    let breadcrumb: StringArray = rows.iter().map(|(r, _)| r.breadcrumb.as_deref()).collect();

    // Nullable int fields
    let split_part_ordinal: Int32Array = rows.iter().map(|(r, _)| r.split_part_ordinal).collect();
    let split_part_count: Int32Array = rows.iter().map(|(r, _)| r.split_part_count).collect();

    let file_complete: BooleanArray = rows.iter().map(|(r, _)| Some(r.file_complete)).collect();

    let columns: Vec<ArrayRef> = vec![
        Arc::new(point_id),
        Arc::new(text),
        vector,
        Arc::new(catalog),
        Arc::new(label_id),
        active_label_ids,
        Arc::new(embedder_id),
        Arc::new(chunker_id),
        Arc::new(blob_id),
        Arc::new(content_hash),
        Arc::new(file_id),
        Arc::new(relative_path),
        Arc::new(package_name),
        Arc::new(source_uri),
        Arc::new(chunk_ordinal),
        Arc::new(chunk_count),
        Arc::new(start_line),
        Arc::new(end_line),
        Arc::new(symbol_name),
        Arc::new(chunk_type),
        Arc::new(chunk_kind),
        Arc::new(breadcrumb),
        Arc::new(split_part_ordinal),
        Arc::new(split_part_count),
        Arc::new(file_complete),
    ];

    RecordBatch::try_new(schema, columns)
        .map_err(|e| anyhow!("Failed to create RecordBatch: {}", e))
}

/// Build a List<Utf8> array from a slice of string slices.
fn build_string_list_array(values: &[&[String]]) -> ArrayRef {
    let mut offsets = Vec::with_capacity(values.len() + 1);
    offsets.push(0i32);

    let mut all_strings = Vec::new();
    for list in values {
        for s in *list {
            all_strings.push(Some(s.as_str()));
        }
        offsets.push(all_strings.len() as i32);
    }

    let inner: StringArray = all_strings.iter().copied().collect();
    let offset_buffer = OffsetBuffer::new(ScalarBuffer::from(offsets));

    Arc::new(
        ListArray::try_new(
            Arc::new(Field::new("item", DataType::Utf8, true)),
            offset_buffer,
            Arc::new(inner),
            None,
        )
        .expect("Failed to create ListArray"),
    )
}

/// Parse a RecordBatch row into a ChunkRow.
///
/// Validates all identifier fields.
fn parse_chunk_row(batch: &RecordBatch, row_idx: usize) -> Result<ChunkRow> {
    let point_id = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("point_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let text = batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("text column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    // Skip vector column (column 2) - we don't need it for the row struct

    let catalog = batch
        .column(3)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("catalog column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let label_id = batch
        .column(4)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("label_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let active_label_ids = {
        let list_array = batch
            .column(5)
            .as_any()
            .downcast_ref::<ListArray>()
            .ok_or_else(|| anyhow!("active_label_ids column is not a ListArray"))?;
        let list_value = list_array.value(row_idx);
        let string_array = list_value
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| anyhow!("active_label_ids inner array is not a StringArray"))?;
        (0..string_array.len())
            .map(|i| string_array.value(i).to_string())
            .collect()
    };

    let embedder_id = batch
        .column(6)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("embedder_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let chunker_id = batch
        .column(7)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("chunker_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let blob_id = batch
        .column(8)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("blob_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let content_hash = batch
        .column(9)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("content_hash column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let file_id = batch
        .column(10)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("file_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let relative_path = batch
        .column(11)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("relative_path column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let package_name = batch
        .column(12)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("package_name column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let source_uri = batch
        .column(13)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("source_uri column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let chunk_ordinal = batch
        .column(14)
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow!("chunk_ordinal column is not an Int32Array"))?
        .value(row_idx);

    let chunk_count = batch
        .column(15)
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow!("chunk_count column is not an Int32Array"))?
        .value(row_idx);

    let start_line = batch
        .column(16)
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow!("start_line column is not an Int32Array"))?
        .value(row_idx);

    let end_line = batch
        .column(17)
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow!("end_line column is not an Int32Array"))?
        .value(row_idx);

    // Nullable string fields
    let symbol_name = {
        let arr = batch
            .column(18)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| anyhow!("symbol_name column is not a StringArray"))?;
        if arr.is_null(row_idx) {
            None
        } else {
            Some(arr.value(row_idx).to_string())
        }
    };

    let chunk_type = batch
        .column(19)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("chunk_type column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let chunk_kind = batch
        .column(20)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("chunk_kind column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let breadcrumb = {
        let arr = batch
            .column(21)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| anyhow!("breadcrumb column is not a StringArray"))?;
        if arr.is_null(row_idx) {
            None
        } else {
            Some(arr.value(row_idx).to_string())
        }
    };

    // Nullable int fields
    let split_part_ordinal = {
        let arr = batch
            .column(22)
            .as_any()
            .downcast_ref::<Int32Array>()
            .ok_or_else(|| anyhow!("split_part_ordinal column is not an Int32Array"))?;
        if arr.is_null(row_idx) {
            None
        } else {
            Some(arr.value(row_idx))
        }
    };

    let split_part_count = {
        let arr = batch
            .column(23)
            .as_any()
            .downcast_ref::<Int32Array>()
            .ok_or_else(|| anyhow!("split_part_count column is not an Int32Array"))?;
        if arr.is_null(row_idx) {
            None
        } else {
            Some(arr.value(row_idx))
        }
    };

    let file_complete = batch
        .column(24)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or_else(|| anyhow!("file_complete column is not a BooleanArray"))?
        .value(row_idx);

    let row = ChunkRow {
        point_id,
        text,
        catalog,
        label_id,
        active_label_ids,
        embedder_id,
        chunker_id,
        blob_id,
        content_hash,
        file_id,
        relative_path,
        package_name,
        source_uri,
        chunk_ordinal,
        chunk_count,
        start_line,
        end_line,
        symbol_name,
        chunk_type,
        chunk_kind,
        breadcrumb,
        split_part_ordinal,
        split_part_count,
        file_complete,
    };

    row.validate()?;
    Ok(row)
}

/// Extract the distance column from a vector search result.
fn extract_distance(batch: &RecordBatch, row_idx: usize) -> Result<f32> {
    // LanceDB returns distance as "_distance" column
    let distance_array = batch
        .column_by_name("_distance")
        .ok_or_else(|| anyhow!("Missing _distance column in vector search result"))?;

    let distances = distance_array
        .as_any()
        .downcast_ref::<Float32Array>()
        .ok_or_else(|| anyhow!("_distance column is not a Float32Array"))?;

    Ok(distances.value(row_idx))
}

/// Chunk storage operations for LanceDB.
pub struct ChunkStorage {
    table: Arc<lancedb::table::Table>,
}

impl ChunkStorage {
    /// Create a new ChunkStorage wrapping a table reference.
    pub fn new(table: Arc<lancedb::table::Table>) -> Self {
        Self { table }
    }

    /// Upsert a batch of chunk rows by point_id.
    ///
    /// Matched rows are updated in place (same point_id implies same content
    /// by construction, since file_id already incorporates blob_id + path +
    /// embedder + chunker).
    pub async fn upsert(&self, rows: &[ChunkRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }

        let schema = self.table.schema().await?;
        let batch = chunk_rows_to_record_batch(rows.iter(), schema.clone())?;

        // Use merge_insert for proper upsert semantics
        let reader = RecordBatchIterator::new(std::iter::once(Ok(batch)), schema);
        let mut builder = self.table.merge_insert(&["point_id"]);
        builder
            .when_matched_update_all(None)
            .when_not_matched_insert_all();
        builder.execute(Box::new(reader)).await?;

        Ok(())
    }

    /// Upsert a batch of chunk rows with their embedding vectors by point_id.
    ///
    /// This is the primary method for writing chunks during crawl, where we have
    /// both the row data and the computed embedding vectors.
    ///
    /// Matched rows are updated in place (same point_id implies same content
    /// by construction, since file_id already incorporates blob_id + path +
    /// embedder + chunker).
    pub async fn upsert_with_vectors(&self, rows: &[ChunkRow], vectors: &[Vec<f32>]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }

        if rows.len() != vectors.len() {
            return Err(anyhow!(
                "Rows and vectors count mismatch: {} vs {}",
                rows.len(),
                vectors.len()
            ));
        }

        let schema = self.table.schema().await?;
        let rows_with_vectors: Vec<(&ChunkRow, &[f32])> = rows
            .iter()
            .zip(vectors.iter().map(|v| v.as_slice()))
            .collect();
        let batch =
            chunk_rows_to_record_batch_with_vectors(rows_with_vectors.into_iter(), schema.clone())?;

        // Use merge_insert for proper upsert semantics
        let reader = RecordBatchIterator::new(std::iter::once(Ok(batch)), schema);
        let mut builder = self.table.merge_insert(&["point_id"]);
        builder
            .when_matched_update_all(None)
            .when_not_matched_insert_all();
        builder.execute(Box::new(reader)).await?;

        Ok(())
    }

    /// Look up a single chunk by point_id.
    ///
    /// Returns None if the chunk doesn't exist.
    pub async fn get_by_point_id(&self, point_id: &str) -> Result<Option<ChunkRow>> {
        let predicate = format!("point_id = '{}'", point_id);

        let results = self
            .table
            .query()
            .only_if(&predicate)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to query chunk by point_id: {}", e))?;

        let batches = results
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| anyhow!("Failed to collect query results: {}", e))?;

        for batch in &batches {
            if batch.num_rows() > 0 {
                let row = parse_chunk_row(batch, 0)?;
                return Ok(Some(row));
            }
        }

        Ok(None)
    }

    /// Return all chunks for a given file_id where active_label_ids contains
    /// the given label, sorted by chunk_ordinal.
    ///
    /// Validates each row.
    pub async fn get_chunks_by_file_id_with_label(
        &self,
        file_id: &str,
        label_id: &str,
    ) -> Result<Vec<ChunkRow>> {
        let predicate = format!(
            "file_id = '{}' AND array_contains(active_label_ids, '{}')",
            file_id, label_id
        );

        let results = self
            .table
            .query()
            .only_if(&predicate)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to query chunks by file_id: {}", e))?;

        let batches = results
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| anyhow!("Failed to collect query results: {}", e))?;

        let mut rows: Vec<ChunkRow> = Vec::new();
        for batch in &batches {
            for i in 0..batch.num_rows() {
                rows.push(parse_chunk_row(batch, i)?);
            }
        }

        // Sort by chunk_ordinal
        rows.sort_by_key(|r| r.chunk_ordinal);

        Ok(rows)
    }

    /// Return all chunks for a given file_id, sorted by chunk_ordinal.
    ///
    /// Does not filter by label; used for label-add operations.
    /// Validates each row.
    pub async fn get_chunks_by_file_id(&self, file_id: &str) -> Result<Vec<ChunkRow>> {
        let predicate = format!("file_id = '{}'", file_id);

        let results = self
            .table
            .query()
            .only_if(&predicate)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to query chunks by file_id: {}", e))?;

        let batches = results
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| anyhow!("Failed to collect query results: {}", e))?;

        let mut rows: Vec<ChunkRow> = Vec::new();
        for batch in &batches {
            for i in 0..batch.num_rows() {
                rows.push(parse_chunk_row(batch, i)?);
            }
        }

        // Sort by chunk_ordinal
        rows.sort_by_key(|r| r.chunk_ordinal);

        Ok(rows)
    }

    /// Vector search: given a query vector, a label filter, and a limit,
    /// return the top-N chunks by cosine similarity that belong to the label.
    ///
    /// Brute-force scan; no ANN index.
    pub async fn vector_search(
        &self,
        query_vector: &[f32],
        label_id: &str,
        limit: usize,
    ) -> Result<Vec<ScoredChunkRow>> {
        let predicate = format!("array_contains(active_label_ids, '{}')", label_id);

        let results = self
            .table
            .query()
            .nearest_to(query_vector)
            .map_err(|e| anyhow!("Failed to set query vector: {}", e))?
            .distance_type(DistanceType::Cosine)
            .only_if(&predicate)
            .limit(limit)
            .execute()
            .await
            .map_err(|e| anyhow!("Vector search failed: {}", e))?;

        let batches = results
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| anyhow!("Failed to collect search results: {}", e))?;

        let mut scored_rows: Vec<ScoredChunkRow> = Vec::new();
        for batch in &batches {
            for i in 0..batch.num_rows() {
                let chunk = parse_chunk_row(batch, i)?;
                let score = extract_distance(batch, i)?;
                scored_rows.push(ScoredChunkRow { chunk, score });
            }
        }

        Ok(scored_rows)
    }

    /// Return all chunks for a given label, with optional chunk_ordinal filter.
    ///
    /// In-memory Vec, not a streaming iterator.
    /// Used by sentinel scans and label reassignment.
    pub async fn get_chunks_for_label(
        &self,
        label_id: &str,
        chunk_ordinal: Option<i32>,
    ) -> Result<Vec<ChunkRow>> {
        let predicate = match chunk_ordinal {
            Some(ordinal) => format!(
                "array_contains(active_label_ids, '{}') AND chunk_ordinal = {}",
                label_id, ordinal
            ),
            None => format!("array_contains(active_label_ids, '{}')", label_id),
        };

        let results = self
            .table
            .query()
            .only_if(&predicate)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to query chunks for label: {}", e))?;

        let batches = results
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| anyhow!("Failed to collect query results: {}", e))?;

        let mut rows: Vec<ChunkRow> = Vec::new();
        for batch in &batches {
            for i in 0..batch.num_rows() {
                rows.push(parse_chunk_row(batch, i)?);
            }
        }

        Ok(rows)
    }

    /// Update the active_label_ids array of a single chunk.
    ///
    /// Uses LanceDB's update() with SQL expression for in-place modification.
    pub async fn update_active_labels(&self, point_id: &str, new_labels: &[String]) -> Result<()> {
        // Build SQL array literal like "['label1', 'label2']"
        let labels_sql = if new_labels.is_empty() {
            "[]".to_string()
        } else {
            let quoted: Vec<String> = new_labels.iter().map(|l| format!("'{}'", l)).collect();
            format!("[{}]", quoted.join(", "))
        };

        let predicate = format!("point_id = '{}'", point_id);

        self.table
            .update()
            .only_if(&predicate)
            .column("active_label_ids", &labels_sql)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to update active_label_ids: {}", e))?;

        Ok(())
    }

    /// Update the file_complete boolean of a single chunk (sentinel marker).
    ///
    /// Uses LanceDB's update() with SQL boolean literal (true/false).
    pub async fn update_file_complete(&self, point_id: &str, complete: bool) -> Result<()> {
        let predicate = format!("point_id = '{}'", point_id);
        let value = if complete { "true" } else { "false" };

        self.table
            .update()
            .only_if(&predicate)
            .column("file_complete", value)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to update file_complete: {}", e))?;

        Ok(())
    }

    /// Batch-delete chunks by a list of point_ids.
    pub async fn delete_by_point_ids(&self, point_ids: &[String]) -> Result<()> {
        if point_ids.is_empty() {
            return Ok(());
        }

        // Build IN clause predicate
        let quoted: Vec<String> = point_ids.iter().map(|id| format!("'{}'", id)).collect();
        let predicate = format!("point_id IN ({})", quoted.join(", "));

        self.table
            .delete(&predicate)
            .await
            .map_err(|e| anyhow!("Failed to delete chunks: {}", e))?;

        Ok(())
    }

    /// Delete all chunks matching a given catalog, returning the count deleted.
    pub async fn delete_by_catalog(&self, catalog: &str) -> Result<u64> {
        let predicate = format!("catalog = '{}'", catalog);

        let count_before = self.table.count_rows(None).await.unwrap_or(0);

        self.table
            .delete(&predicate)
            .await
            .map_err(|e| anyhow!("Failed to delete chunks by catalog: {}", e))?;

        let count_after = self.table.count_rows(None).await.unwrap_or(0);

        Ok(count_before.saturating_sub(count_after) as u64)
    }

    /// Truncate the table (empty all rows, preserve schema).
    pub async fn truncate(&self) -> Result<()> {
        self.table
            .delete("true")
            .await
            .map_err(|e| anyhow!("Failed to truncate chunks table: {}", e))?;

        Ok(())
    }

    /// Get the table reference.
    pub fn table(&self) -> Arc<lancedb::table::Table> {
        self.table.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::schema::chunks_schema;
    use lancedb::connect;
    use tempfile::TempDir;

    async fn create_test_storage() -> (TempDir, ChunkStorage) {
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("test_db");

        let db = connect(db_path.to_str().unwrap())
            .execute()
            .await
            .expect("Failed to create database");

        let schema = chunks_schema();
        let table = db
            .create_empty_table("chunks", schema)
            .execute()
            .await
            .expect("Failed to create table");

        (tmp_dir, ChunkStorage::new(Arc::new(table)))
    }

    fn test_chunk_row(point_id: &str, file_id: &str, ordinal: i32) -> ChunkRow {
        ChunkRow {
            point_id: point_id.to_string(),
            text: format!("Test content for {}", point_id),
            catalog: "test-catalog".to_string(),
            label_id: "test-catalog:main".to_string(),
            active_label_ids: vec!["test-catalog:main".to_string()],
            embedder_id: "test-embedder:v1".to_string(),
            chunker_id: "test-chunker:v1".to_string(),
            blob_id: "abc123".to_string(),
            content_hash: "def456".to_string(),
            file_id: file_id.to_string(),
            relative_path: "src/test.ts".to_string(),
            package_name: "test-package".to_string(),
            source_uri: "/path/to/test.ts".to_string(),
            chunk_ordinal: ordinal,
            chunk_count: 3,
            start_line: 1,
            end_line: 50,
            symbol_name: Some("testFunction".to_string()),
            chunk_type: "function".to_string(),
            chunk_kind: "content".to_string(),
            breadcrumb: Some("test-package:test.ts:testFunction".to_string()),
            split_part_ordinal: None,
            split_part_count: None,
            file_complete: ordinal == 1,
        }
    }

    #[tokio::test]
    async fn test_upsert_and_get() {
        let (_tmp_dir, storage) = create_test_storage().await;

        let row = test_chunk_row("file1:1", "file1", 1);
        storage.upsert(std::slice::from_ref(&row)).await.unwrap();

        let retrieved = storage.get_by_point_id("file1:1").await.unwrap();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.point_id, "file1:1");
        assert_eq!(retrieved.text, row.text);
    }

    #[tokio::test]
    async fn test_get_nonexistent() {
        let (_tmp_dir, storage) = create_test_storage().await;

        let retrieved = storage.get_by_point_id("nonexistent:1").await.unwrap();
        assert!(retrieved.is_none());
    }

    #[tokio::test]
    async fn test_get_chunks_by_file_id_with_label() {
        let (_tmp_dir, storage) = create_test_storage().await;

        // Insert chunks for file1
        storage
            .upsert(&[test_chunk_row("file1:1", "file1", 1)])
            .await
            .unwrap();
        storage
            .upsert(&[test_chunk_row("file1:2", "file1", 2)])
            .await
            .unwrap();
        storage
            .upsert(&[test_chunk_row("file1:3", "file1", 3)])
            .await
            .unwrap();

        // Insert chunk for different file
        storage
            .upsert(&[test_chunk_row("file2:1", "file2", 1)])
            .await
            .unwrap();

        let chunks = storage
            .get_chunks_by_file_id_with_label("file1", "test-catalog:main")
            .await
            .unwrap();

        assert_eq!(chunks.len(), 3);
        // Verify sorted by ordinal
        assert_eq!(chunks[0].chunk_ordinal, 1);
        assert_eq!(chunks[1].chunk_ordinal, 2);
        assert_eq!(chunks[2].chunk_ordinal, 3);
    }

    #[tokio::test]
    async fn test_get_chunks_for_label() {
        let (_tmp_dir, storage) = create_test_storage().await;

        storage
            .upsert(&[test_chunk_row("file1:1", "file1", 1)])
            .await
            .unwrap();
        storage
            .upsert(&[test_chunk_row("file1:2", "file1", 2)])
            .await
            .unwrap();

        let chunks = storage
            .get_chunks_for_label("test-catalog:main", None)
            .await
            .unwrap();

        assert_eq!(chunks.len(), 2);
    }

    #[tokio::test]
    async fn test_get_chunks_for_label_with_ordinal_filter() {
        let (_tmp_dir, storage) = create_test_storage().await;

        storage
            .upsert(&[test_chunk_row("file1:1", "file1", 1)])
            .await
            .unwrap();
        storage
            .upsert(&[test_chunk_row("file1:2", "file1", 2)])
            .await
            .unwrap();

        let chunks = storage
            .get_chunks_for_label("test-catalog:main", Some(1))
            .await
            .unwrap();

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_ordinal, 1);
    }

    #[tokio::test]
    async fn test_update_active_labels() {
        let (_tmp_dir, storage) = create_test_storage().await;

        storage
            .upsert(&[test_chunk_row("file1:1", "file1", 1)])
            .await
            .unwrap();

        // Add another label
        storage
            .update_active_labels(
                "file1:1",
                &[
                    "test-catalog:main".to_string(),
                    "test-catalog:feature".to_string(),
                ],
            )
            .await
            .unwrap();

        let retrieved = storage.get_by_point_id("file1:1").await.unwrap().unwrap();
        assert_eq!(retrieved.active_label_ids.len(), 2);
        assert!(
            retrieved
                .active_label_ids
                .contains(&"test-catalog:main".to_string())
        );
        assert!(
            retrieved
                .active_label_ids
                .contains(&"test-catalog:feature".to_string())
        );
    }

    #[tokio::test]
    async fn test_delete_by_point_ids() {
        let (_tmp_dir, storage) = create_test_storage().await;

        storage
            .upsert(&[test_chunk_row("file1:1", "file1", 1)])
            .await
            .unwrap();
        storage
            .upsert(&[test_chunk_row("file1:2", "file1", 2)])
            .await
            .unwrap();

        storage
            .delete_by_point_ids(&["file1:1".to_string()])
            .await
            .unwrap();

        let retrieved1 = storage.get_by_point_id("file1:1").await.unwrap();
        let retrieved2 = storage.get_by_point_id("file1:2").await.unwrap();

        assert!(retrieved1.is_none());
        assert!(retrieved2.is_some());
    }

    #[tokio::test]
    async fn test_delete_by_catalog() {
        let (_tmp_dir, storage) = create_test_storage().await;

        storage
            .upsert(&[test_chunk_row("file1:1", "file1", 1)])
            .await
            .unwrap();
        storage
            .upsert(&[test_chunk_row("file2:1", "file2", 1)])
            .await
            .unwrap();

        let count = storage.delete_by_catalog("test-catalog").await.unwrap();
        assert_eq!(count, 2);

        let chunks = storage
            .get_chunks_for_label("test-catalog:main", None)
            .await
            .unwrap();
        assert_eq!(chunks.len(), 0);
    }

    #[tokio::test]
    async fn test_truncate() {
        let (_tmp_dir, storage) = create_test_storage().await;

        storage
            .upsert(&[test_chunk_row("file1:1", "file1", 1)])
            .await
            .unwrap();
        storage
            .upsert(&[test_chunk_row("file2:1", "file2", 1)])
            .await
            .unwrap();

        storage.truncate().await.unwrap();

        let count = storage.table.count_rows(None).await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_upsert_overwrites() {
        let (_tmp_dir, storage) = create_test_storage().await;

        // Insert initial row
        let mut row = test_chunk_row("file1:1", "file1", 1);
        row.text = "Original text".to_string();
        storage.upsert(std::slice::from_ref(&row)).await.unwrap();

        // Upsert with updated text
        row.text = "Updated text".to_string();
        storage.upsert(std::slice::from_ref(&row)).await.unwrap();

        let retrieved = storage.get_by_point_id("file1:1").await.unwrap().unwrap();
        assert_eq!(retrieved.text, "Updated text");

        // Verify only one row exists for this point_id
        let chunks = storage
            .get_chunks_for_label("test-catalog:main", Some(1))
            .await
            .unwrap();
        assert_eq!(chunks.len(), 1);
    }
}
