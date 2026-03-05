//! Embedding generation using ONNX Runtime
//! 
//! This module handles loading jina-embeddings-v2-base-code via ONNX Runtime
//! for generating 768-dimensional embeddings optimized for code.

use anyhow::Result;
use ort::session::{Session, builder::GraphOptimizationLevel};
use ort::value::Tensor;
#[cfg(feature = "coreml")]
use ort::execution_providers::CoreML;
#[cfg(feature = "coreml")]
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
        #[allow(unused_mut)]
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

    /// Generates embeddings for multiple texts in a single batch
    /// 
    /// More efficient than individual encode() calls due to GPU parallelism.
    /// Texts are padded to the same length for batching.
    pub fn encode_batch(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Tokenize all texts
        let encodings: Vec<_> = texts
            .iter()
            .map(|text| {
                self.tokenizer
                    .encode(*text, true)
                    .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))
            })
            .collect::<Result<Vec<_>>>()?;

        // Find max length (truncate to MAX_LENGTH)
        let max_seq_len = encodings
            .iter()
            .map(|e| e.get_ids().len().min(MAX_LENGTH))
            .max()
            .unwrap_or(1);

        let batch_size = texts.len();
        let hidden_size = 768usize;

        // Create padded batch tensors
        let mut input_ids: Vec<i64> = vec![0; batch_size * max_seq_len];
        let mut attention_mask: Vec<i64> = vec![0; batch_size * max_seq_len];

        for (i, encoding) in encodings.iter().enumerate() {
            let ids = encoding.get_ids();
            let mask = encoding.get_attention_mask();
            let seq_len = ids.len().min(MAX_LENGTH);

            for j in 0..seq_len {
                input_ids[i * max_seq_len + j] = ids[j] as i64;
                attention_mask[i * max_seq_len + j] = mask[j] as i64;
            }
        }

        // Run batch inference
        let outputs = self.session.run(ort::inputs![
            "input_ids" => Tensor::from_array(([batch_size, max_seq_len], input_ids.clone()))?,
            "attention_mask" => Tensor::from_array(([batch_size, max_seq_len], attention_mask.clone()))?,
        ])?;

        // Extract output tensor
        let (_shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        // shape is [batch_size, max_seq_len, 768]

        // Mean pooling for each sequence in batch
        let mut embeddings = Vec::with_capacity(batch_size);
        
        for i in 0..batch_size {
            let encoding = &encodings[i];
            let seq_len = encoding.get_ids().len().min(MAX_LENGTH);
            
            let embedding: Vec<f32> = (0..hidden_size)
                .map(|h| {
                    (0..seq_len)
                        .map(|j| data[i * max_seq_len * hidden_size + j * hidden_size + h])
                        .sum::<f32>() / seq_len as f32
                })
                .collect();
            
            embeddings.push(embedding);
        }

        Ok(embeddings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_encode_single() {
        let mut emb_gen = EmbeddingGenerator::new().unwrap();
        let result = emb_gen.encode("function test() { return 42; }");
        assert!(result.is_ok());
        let embedding = result.unwrap();
        assert_eq!(embedding.len(), 768);
    }
    
    #[test]
    fn test_encode_batch_small() {
        let mut emb_gen = EmbeddingGenerator::new().unwrap();
        let texts: Vec<&str> = vec!["function a() {}", "function b() {}", "function c() {}"];
        let result = emb_gen.encode_batch(&texts);
        assert!(result.is_ok());
        let embeddings = result.unwrap();
        assert_eq!(embeddings.len(), 3);
        for emb in embeddings {
            assert_eq!(emb.len(), 768);
        }
    }
    
    #[test]
    fn test_encode_batch_64() {
        let mut emb_gen = EmbeddingGenerator::new().unwrap();
        let texts: Vec<&str> = (0..64).map(|_| "function test() { return 42; }").collect();
        let start = std::time::Instant::now();
        let result = emb_gen.encode_batch(&texts);
        let elapsed = start.elapsed();
        println!("Batch of 64 took {:?}", elapsed);
        assert!(result.is_ok());
        let embeddings = result.unwrap();
        assert_eq!(embeddings.len(), 64);
    }
}
