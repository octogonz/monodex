//! Embedding generation using ONNX Runtime
//! 
//! This module handles loading jina-embeddings-v2-base-code via ONNX Runtime
//! for generating 768-dimensional embeddings optimized for code.

use anyhow::Result;
use ort::session::{Session, builder::GraphOptimizationLevel};
use ort::value::Tensor;
use ort::execution_providers::CoreML;
use ort::ep::ExecutionProvider;
use tokenizers::Tokenizer;
use hf_hub::{api::sync::Api, Repo, RepoType};

const MODEL_ID: &str = "jinaai/jina-embeddings-v2-base-code";
const MAX_LENGTH: usize = 8192;

/// Generates embeddings using jina-embeddings-v2-base-code via ONNX Runtime
pub struct EmbeddingGenerator {
    session: Session,
    tokenizer: Tokenizer,
}

impl EmbeddingGenerator {
    /// Creates a new embedding generator
    /// 
    /// Downloads the ONNX model and tokenizer from HuggingFace Hub if not cached.
    /// Uses CoreML (GPU) on Apple Silicon, falls back to CPU otherwise.
    pub fn new() -> Result<Self> {
        // Download model files from HuggingFace (cached locally after first download)
        let api = Api::new()?;
        let repo = Repo::new(MODEL_ID.to_string(), RepoType::Model);
        let api = api.repo(repo);

        let tokenizer_path = api.get("tokenizer.json")?;
        let onnx_path = api.get("onnx/model.onnx")?;

        // Load tokenizer
        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {}", e))?;

        // Build session with CoreML if available
        let mut builder = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::All)?;
        
        // Try CoreML execution provider (Apple Silicon GPU)
        #[cfg(feature = "coreml")]
        {
            let coreml = CoreML::default();
            match coreml.register(&mut builder) {
                Ok(()) => println!("Using CoreML GPU acceleration"),
                Err(e) => eprintln!("CoreML not available ({}), falling back to CPU", e),
            }
        }
        
        let session = builder.commit_from_file(&onnx_path)?;

        Ok(Self { session, tokenizer })
    }

    /// Generates an embedding vector for the given text
    /// 
    /// Automatically truncates to 8192 tokens (model's maximum).
    /// Returns a 768-dimensional vector suitable for semantic code search.
    pub fn encode(&mut self, text: &str) -> Result<Vec<f32>> {
        // Tokenize
        let encoding = self.tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))?;

        let ids = encoding.get_ids();
        let attention_mask = encoding.get_attention_mask();

        // Truncate if needed
        let seq_len = ids.len().min(MAX_LENGTH);

        // Create input tensors using (shape, data) tuple format
        let input_ids: Vec<i64> = ids[..seq_len].iter().map(|&id| id as i64).collect();
        let attention_mask_data: Vec<i64> = attention_mask[..seq_len].iter().map(|&m| m as i64).collect();

        // Run inference
        let outputs = self.session.run(ort::inputs![
            "input_ids" => Tensor::from_array(([1, seq_len], input_ids))?,
            "attention_mask" => Tensor::from_array(([1, seq_len], attention_mask_data))?,
        ])?;

        // Extract output tensor - returns (shape, data) tuple
        let (_shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        
        // shape is [1, seq_len, 768]
        let hidden_size = 768usize;
        
        // Mean pooling over sequence dimension
        let embedding: Vec<f32> = (0..hidden_size)
            .map(|i| {
                (0..seq_len)
                    .map(|j| data[j * hidden_size + i])
                    .sum::<f32>() / seq_len as f32
            })
            .collect();

        Ok(embedding)
    }
}
