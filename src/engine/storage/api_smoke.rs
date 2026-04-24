//! LanceDB API verification tests.
//!
//! Purpose: Verify the specific LanceDB operations the migration depends on, before
//! the storage module commits to them in bulk.
//!
//! ## Key findings (update as discoveries are made):
//!
//! 1. **Array-contains filter**: Use SQL `array_contains(col_name, 'value')`
//!    to filter List<Utf8> columns for rows that contain a specific value.
//!
//! 2. **Update array column**: LanceDB supports partial updates via `update()`
//!    with a predicate. For list-typed columns, pass an SQL array literal like `['a', 'b']`.
//!
//! 3. **Vector search metric**: Cosine similarity is configured via
//!    `.distance_type(lancedb::DistanceType::Cosine)`. Results are sorted by
//!    distance ascending (smaller = more similar).
//!
//! 4. **Truncate table**: Use `delete("true")` to remove all rows while preserving schema.
//!
//! 5. **Async API**: All operations are async via tokio runtime.
//!
//! 6. **Create empty table**: Use `db.create_empty_table(name, schema)` to create
//!    a table with schema but no rows.

use arrow_array::{
    Array, ArrayRef, FixedSizeListArray, Float32Array, ListArray, RecordBatch, StringArray,
};
use arrow_buffer::{OffsetBuffer, ScalarBuffer};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::{DistanceType, connect};
use std::sync::Arc;
use tempfile::TempDir;

/// Create a 768-dim vector filled with a constant value.
fn make_constant_vector(value: f32) -> ArrayRef {
    let values: Float32Array = std::iter::repeat_n(value, 768).collect();
    let field = Arc::new(Field::new("item", DataType::Float32, true));
    Arc::new(FixedSizeListArray::new(field, 768, Arc::new(values), None)) as ArrayRef
}

/// Create a List<Utf8> array from vec of string slices for a single row.
fn make_string_list_single(strings: Vec<&str>) -> ArrayRef {
    let inner: StringArray = strings.iter().map(Some).collect();
    let offsets: OffsetBuffer<i32> =
        OffsetBuffer::new(ScalarBuffer::from(vec![0i32, inner.len() as i32]));
    Arc::new(
        ListArray::try_new(
            Arc::new(Field::new("item", DataType::Utf8, true)),
            offsets,
            Arc::new(inner),
            None,
        )
        .unwrap(),
    ) as ArrayRef
}

// =============================================================================
// TEST 1: Basic table creation and insertion
// =============================================================================

#[tokio::test]
async fn test_create_table_and_insert() {
    let tmp_dir = TempDir::new().unwrap();
    let db_path = tmp_dir.path().join("test_db");
    let db = connect(db_path.to_str().unwrap())
        .execute()
        .await
        .expect("Failed to open database");

    // Schema with point_id (string PK), active_label_ids (List<Utf8>), vector (768-dim)
    let schema = Arc::new(Schema::new(vec![
        Field::new("point_id", DataType::Utf8, false),
        Field::new(
            "active_label_ids",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            true,
        ),
        Field::new(
            "vector",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), 768),
            true,
        ),
    ]));

    let table = db
        .create_empty_table("chunks", schema.clone())
        .execute()
        .await
        .expect("Failed to create table");

    // Insert a row
    let point_id: StringArray = std::iter::once(Some("file123:1")).collect();
    let label_ids = make_string_list_single(vec!["catalog:main", "catalog:feature"]);
    let vector = make_constant_vector(0.5);

    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(point_id) as ArrayRef, label_ids, vector],
    )
    .expect("Failed to create record batch");

    table
        .add(batch)
        .execute()
        .await
        .expect("Failed to insert row");

    // Verify count
    let count = table.count_rows(None).await.expect("Failed to count rows");
    assert_eq!(count, 1);
}

// =============================================================================
// TEST 2: Filter on List<Utf8> column containing a string
// =============================================================================

#[tokio::test]
async fn test_filter_list_contains() {
    let tmp_dir = TempDir::new().unwrap();
    let db_path = tmp_dir.path().join("test_db");
    let db = connect(db_path.to_str().unwrap())
        .execute()
        .await
        .expect("Failed to open database");

    let schema = Arc::new(Schema::new(vec![
        Field::new("point_id", DataType::Utf8, false),
        Field::new(
            "active_label_ids",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            true,
        ),
    ]));

    let table = db
        .create_empty_table("chunks", schema.clone())
        .execute()
        .await
        .expect("Failed to create table");

    // Insert 3 rows with different label memberships
    // Row 0: ["catalog:main", "catalog:feature"]
    // Row 1: ["catalog:main"]
    // Row 2: ["catalog:other"]
    let point_id: StringArray = vec!["file1:1", "file2:1", "file3:1"]
        .into_iter()
        .map(Some)
        .collect();

    // Build a ListArray with 3 rows
    let label_ids = Arc::new(
        ListArray::try_new(
            Arc::new(Field::new("item", DataType::Utf8, true)),
            OffsetBuffer::new(ScalarBuffer::from(vec![0i32, 2, 3, 4])),
            Arc::new(StringArray::from(vec![
                Some("catalog:main"),
                Some("catalog:feature"),
                Some("catalog:main"),
                Some("catalog:other"),
            ])),
            None,
        )
        .unwrap(),
    ) as ArrayRef;

    let batch = RecordBatch::try_new(schema, vec![Arc::new(point_id) as ArrayRef, label_ids])
        .expect("Failed to create record batch");

    table
        .add(batch)
        .execute()
        .await
        .expect("Failed to insert rows");

    // Filter: rows where active_label_ids contains "catalog:main"
    // Use SQL array_contains function
    let results = table
        .query()
        .only_if("array_contains(active_label_ids, 'catalog:main')")
        .execute()
        .await
        .expect("Query failed");

    let batches = results
        .try_collect::<Vec<_>>()
        .await
        .expect("Collect failed");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 2, "Should find 2 rows with catalog:main label");
}

// =============================================================================
// TEST 3: Update array column value on a single row
// =============================================================================

#[tokio::test]
async fn test_update_array_column() {
    let tmp_dir = TempDir::new().unwrap();
    let db_path = tmp_dir.path().join("test_db");
    let db = connect(db_path.to_str().unwrap())
        .execute()
        .await
        .expect("Failed to open database");

    let schema = Arc::new(Schema::new(vec![
        Field::new("point_id", DataType::Utf8, false),
        Field::new(
            "active_label_ids",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            true,
        ),
    ]));

    let table = db
        .create_empty_table("chunks", schema.clone())
        .execute()
        .await
        .expect("Failed to create table");

    // Insert a row
    let point_id: StringArray = vec!["file1:1"].into_iter().map(Some).collect();
    let label_ids = Arc::new(
        ListArray::try_new(
            Arc::new(Field::new("item", DataType::Utf8, true)),
            OffsetBuffer::new(ScalarBuffer::from(vec![0i32, 2])),
            Arc::new(StringArray::from(vec![
                Some("catalog:main"),
                Some("catalog:feature"),
            ])),
            None,
        )
        .unwrap(),
    ) as ArrayRef;

    let batch = RecordBatch::try_new(schema, vec![Arc::new(point_id) as ArrayRef, label_ids])
        .expect("Failed to create record batch");

    table
        .add(batch)
        .execute()
        .await
        .expect("Failed to insert row");

    // Update the active_label_ids for this row
    table
        .update()
        .only_if("point_id = 'file1:1'")
        .column("active_label_ids", "['catalog:updated', 'catalog:new']")
        .execute()
        .await
        .expect("Failed to update");

    // Verify the update
    let results = table
        .query()
        .only_if("point_id = 'file1:1'")
        .execute()
        .await
        .expect("Query failed");

    let batches = results
        .try_collect::<Vec<_>>()
        .await
        .expect("Collect failed");
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    let labels = batch
        .column(1)
        .as_any()
        .downcast_ref::<ListArray>()
        .expect("Expected ListArray");

    // The updated list should have 2 items: "catalog:updated" and "catalog:new"
    let first_list = labels.value(0);
    let strings = first_list
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("Expected StringArray");
    assert_eq!(strings.len(), 2);
    assert_eq!(strings.value(0), "catalog:updated");
    assert_eq!(strings.value(1), "catalog:new");
}

// =============================================================================
// TEST 4: Vector search with label filter
// =============================================================================

#[tokio::test]
async fn test_vector_search_with_filter() {
    let tmp_dir = TempDir::new().unwrap();
    let db_path = tmp_dir.path().join("test_db");
    let db = connect(db_path.to_str().unwrap())
        .execute()
        .await
        .expect("Failed to open database");

    let schema = Arc::new(Schema::new(vec![
        Field::new("point_id", DataType::Utf8, false),
        Field::new(
            "active_label_ids",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            true,
        ),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                128, // smaller dimension for test
            ),
            true,
        ),
    ]));

    let table = db
        .create_empty_table("chunks", schema.clone())
        .execute()
        .await
        .expect("Failed to create table");

    // Insert rows with different vectors and labels
    // Row 1: vector all 1.0s, has label "a"
    // Row 2: vector all 0.0s, has label "b"
    let point_id: StringArray = vec!["p1", "p2"].into_iter().map(Some).collect();

    let label_ids = Arc::new(
        ListArray::try_new(
            Arc::new(Field::new("item", DataType::Utf8, true)),
            OffsetBuffer::new(ScalarBuffer::from(vec![0i32, 1, 2])),
            Arc::new(StringArray::from(vec![Some("a"), Some("b")])),
            None,
        )
        .unwrap(),
    ) as ArrayRef;

    // Create two 128-dim vectors concatenated
    let v1: Vec<f32> = std::iter::repeat_n(1.0f32, 128).collect();
    let v2: Vec<f32> = std::iter::repeat_n(0.0f32, 128).collect();
    let all_values: Vec<f32> = v1.into_iter().chain(v2.into_iter()).collect();
    let values = Float32Array::from(all_values);
    let field = Arc::new(Field::new("item", DataType::Float32, true));
    let vectors = Arc::new(FixedSizeListArray::new(field, 128, Arc::new(values), None)) as ArrayRef;

    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(point_id) as ArrayRef, label_ids, vectors],
    )
    .expect("Failed to create record batch");

    table
        .add(batch)
        .execute()
        .await
        .expect("Failed to insert rows");

    // Note: Not creating index since we only have 2 rows (PQ needs 256+)
    // Vector search will still work, just without an index

    // Search with filter: query vector all 1.0s, should match p1, but filter to only label "a"
    let query_vec: Vec<f32> = std::iter::repeat_n(1.0f32, 128).collect();

    let results = table
        .query()
        .nearest_to(&query_vec[..])
        .expect("Failed to set query vector")
        .distance_type(DistanceType::Cosine)
        .only_if("array_contains(active_label_ids, 'a')")
        .execute()
        .await
        .expect("Search failed");

    let batches = results
        .try_collect::<Vec<_>>()
        .await
        .expect("Collect failed");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();

    // Should only get p1 (has label "a" and similar vector)
    assert_eq!(total_rows, 1);
}

// =============================================================================
// TEST 5: Truncate table (delete all rows, preserve schema)
// =============================================================================

#[tokio::test]
async fn test_truncate_table() {
    let tmp_dir = TempDir::new().unwrap();
    let db_path = tmp_dir.path().join("test_db");
    let db = connect(db_path.to_str().unwrap())
        .execute()
        .await
        .expect("Failed to open database");

    let schema = Arc::new(Schema::new(vec![
        Field::new("label_id", DataType::Utf8, false),
        Field::new("catalog", DataType::Utf8, false),
    ]));

    let table = db
        .create_empty_table("labels", schema.clone())
        .execute()
        .await
        .expect("Failed to create table");

    // Insert some rows
    let label_id: StringArray = vec!["label1", "label2"].into_iter().map(Some).collect();
    let catalog: StringArray = vec!["cat1", "cat1"].into_iter().map(Some).collect();

    let batch = RecordBatch::try_new(schema, vec![Arc::new(label_id), Arc::new(catalog)])
        .expect("Failed to create record batch");

    table
        .add(batch)
        .execute()
        .await
        .expect("Failed to insert rows");

    assert_eq!(table.count_rows(None).await.unwrap(), 2);

    // Truncate using delete with always-true predicate
    table.delete("true").await.expect("Failed to truncate");

    assert_eq!(table.count_rows(None).await.unwrap(), 0);

    // Table still exists with same schema
    let schema_after = table.schema().await.expect("Failed to get schema");
    assert_eq!(schema_after.fields().len(), 2);
}
