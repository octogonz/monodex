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
//!
//! 7. **Vector search correctness verified**: Unit vectors along specific axes
//!    confirm cosine similarity is computed correctly. Results sorted by distance
//!    ascending (smaller = more similar). Query vectors are normalized internally.

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
// TEST 5: Vector search correctness with hand-crafted vectors
// =============================================================================

/// Test vector search correctness with hand-crafted vectors of known orientation.
///
/// This test catches the "dot product vs cosine on unnormalized vectors" class of bug
/// that structural tests would miss. We use unit vectors along specific axes and verify
/// that cosine similarity returns results in the expected order.
///
/// Setup:
/// - Insert 10 rows with 4-dimensional unit vectors pointing along different axes
/// - Query with a vector at 45° between axes
/// - Verify results are ranked by cosine similarity (dot product for unit vectors)
#[tokio::test]
async fn test_vector_search_correctness() {
    let tmp_dir = TempDir::new().unwrap();
    let db_path = tmp_dir.path().join("test_db");
    let db = connect(db_path.to_str().unwrap())
        .execute()
        .await
        .expect("Failed to open database");

    // Use 4-dimensional vectors for simplicity
    let dim = 4i32;
    let schema = Arc::new(Schema::new(vec![
        Field::new("point_id", DataType::Utf8, false),
        Field::new(
            "active_label_ids",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            true,
        ),
        Field::new(
            "vector",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), dim),
            true,
        ),
    ]));

    let table = db
        .create_empty_table("chunks", schema.clone())
        .execute()
        .await
        .expect("Failed to create table");

    // Create 10 unit vectors along different axes and directions:
    // v0: [1, 0, 0, 0]  - points along axis 0
    // v1: [0, 1, 0, 0]  - points along axis 1
    // v2: [0, 0, 1, 0]  - points along axis 2
    // v3: [0, 0, 0, 1]  - points along axis 3
    // v4: [-1, 0, 0, 0] - opposite of v0
    // v5: [0, -1, 0, 0] - opposite of v1
    // v6: [0, 0, -1, 0] - opposite of v2
    // v7: [0, 0, 0, -1] - opposite of v3
    // v8: [0.707, 0.707, 0, 0] - 45° between axes 0 and 1 (normalized)
    // v9: [0.5, 0.5, 0.5, 0.5] - equally along all axes (normalized)
    let vectors: Vec<Vec<f32>> = vec![
        vec![1.0, 0.0, 0.0, 0.0],   // v0
        vec![0.0, 1.0, 0.0, 0.0],   // v1
        vec![0.0, 0.0, 1.0, 0.0],   // v2
        vec![0.0, 0.0, 0.0, 1.0],   // v3
        vec![-1.0, 0.0, 0.0, 0.0],  // v4
        vec![0.0, -1.0, 0.0, 0.0],  // v5
        vec![0.0, 0.0, -1.0, 0.0],  // v6
        vec![0.0, 0.0, 0.0, -1.0],  // v7
        vec![0.707, 0.707, 0.0, 0.0], // v8 (normalized: sqrt(2)/2 ≈ 0.707)
        vec![0.5, 0.5, 0.5, 0.5],   // v9 (normalized: 0.5 = 1/sqrt(4) = 0.5)
    ];

    let point_ids: Vec<String> = (0..10).map(|i| format!("v{i}")).collect();
    let point_id: StringArray = point_ids.iter().map(|s| Some(s.as_str())).collect();

    // All rows get the same label so we can filter
    let label_ids = Arc::new(
        ListArray::try_new(
            Arc::new(Field::new("item", DataType::Utf8, true)),
            OffsetBuffer::new(ScalarBuffer::from((0..=10).map(|i| i as i32).collect::<Vec<_>>())),
            Arc::new(StringArray::from(vec![Some("test"); 10])),
            None,
        )
        .unwrap(),
    ) as ArrayRef;

    // Flatten vectors into a single array for FixedSizeListArray
    let all_values: Vec<f32> = vectors.iter().flat_map(|v| v.iter().copied()).collect();
    let values = Float32Array::from(all_values);
    let field = Arc::new(Field::new("item", DataType::Float32, true));
    let vectors_array = Arc::new(FixedSizeListArray::new(field, dim, Arc::new(values), None)) as ArrayRef;

    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(point_id) as ArrayRef, label_ids, vectors_array],
    )
    .expect("Failed to create record batch");

    table
        .add(batch)
        .execute()
        .await
        .expect("Failed to insert rows");

    // Test 1: Query with [1, 0, 0, 0] - should rank v0 first (cosine = 1.0)
    // and v4 last (cosine = -1.0)
    let query = vec![1.0f32, 0.0, 0.0, 0.0];
    let results = table
        .query()
        .nearest_to(&query[..])
        .expect("Failed to set query vector")
        .distance_type(DistanceType::Cosine)
        .limit(10)
        .execute()
        .await
        .expect("Search failed");

    let batches = results
        .try_collect::<Vec<_>>()
        .await
        .expect("Collect failed");
    
    // Extract point_ids in order of distance
    let mut ordered_ids: Vec<String> = Vec::new();
    for batch in &batches {
        let ids = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("Expected StringArray");
        for i in 0..ids.len() {
            ordered_ids.push(ids.value(i).to_string());
        }
    }

    // v0 should be first (distance ~0, cosine similarity = 1)
    assert_eq!(ordered_ids.first().map(|s| s.as_str()), Some("v0"), 
        "v0 should be ranked first for query [1,0,0,0]");
    
    // v4 should be last (distance ~2, cosine similarity = -1)
    assert_eq!(ordered_ids.last().map(|s| s.as_str()), Some("v4"),
        "v4 should be ranked last for query [1,0,0,0]");

    // Test 2: Query with [0.707, 0.707, 0, 0] - should rank v0, v1, v8 at top
    // v8 has the same direction (cosine = 1.0)
    // v0 and v1 have cosine = 0.707
    let query = vec![0.707f32, 0.707, 0.0, 0.0];
    let results = table
        .query()
        .nearest_to(&query[..])
        .expect("Failed to set query vector")
        .distance_type(DistanceType::Cosine)
        .limit(10)
        .execute()
        .await
        .expect("Search failed");

    let batches = results
        .try_collect::<Vec<_>>()
        .await
        .expect("Collect failed");
    
    let mut ordered_ids: Vec<String> = Vec::new();
    for batch in &batches {
        let ids = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("Expected StringArray");
        for i in 0..ids.len() {
            ordered_ids.push(ids.value(i).to_string());
        }
    }

    // v8 should be first (exact match, cosine = 1)
    assert_eq!(ordered_ids.first().map(|s| s.as_str()), Some("v8"),
        "v8 should be ranked first for query at 45° between axes 0 and 1");

    // v0 and v1 should be next (both have cosine = 0.707 with the query)
    // They may appear in any order, but both should be in top 4
    let top_4: Vec<&str> = ordered_ids.iter().take(4).map(|s| s.as_str()).collect();
    assert!(top_4.contains(&"v0") && top_4.contains(&"v1"),
        "v0 and v1 should both be in top 4 for query at 45° between axes 0 and 1");

    // Test 3: Query with [1, 1, 1, 1] (not normalized) - should still work correctly
    // For cosine similarity, the query vector is normalized internally
    // v9 should be ranked first (cosine = 1.0)
    let query = vec![1.0f32, 1.0, 1.0, 1.0];
    let results = table
        .query()
        .nearest_to(&query[..])
        .expect("Failed to set query vector")
        .distance_type(DistanceType::Cosine)
        .limit(10)
        .execute()
        .await
        .expect("Search failed");

    let batches = results
        .try_collect::<Vec<_>>()
        .await
        .expect("Collect failed");
    
    let mut ordered_ids: Vec<String> = Vec::new();
    for batch in &batches {
        let ids = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("Expected StringArray");
        for i in 0..ids.len() {
            ordered_ids.push(ids.value(i).to_string());
        }
    }

    // v9 should be first (all components equal, normalized)
    assert_eq!(ordered_ids.first().map(|s| s.as_str()), Some("v9"),
        "v9 should be ranked first for query [1,1,1,1] (equal components)");

    // v4-v7 should be last (opposite directions on at least one axis)
    let last_4: Vec<&str> = ordered_ids.iter().rev().take(4).map(|s| s.as_str()).collect();
    assert!(last_4.contains(&"v4") || last_4.contains(&"v5") || last_4.contains(&"v6") || last_4.contains(&"v7"),
        "At least one negative-axis vector should be in the bottom 4");
}

// =============================================================================
// TEST 6: Truncate table (delete all rows, preserve schema)
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
