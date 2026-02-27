//! Embedding generation using Candle ML framework
//! 
//! This module handles loading the bge-small-en-v1.5 model and generating
//! 384-dimensional embeddings for text chunks.

use anyhow::Result;
use candle_core::{Device, Tensor};
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use candle_nn::VarBuilder;
use hf_hub::{api::sync::Api, Repo, RepoType};
use std::fs;
use tokenizers::Tokenizer;

/// Generates embeddings using the BAAI/bge-small-en-v1.5 model
pub struct EmbeddingGenerator {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl EmbeddingGenerator {
    /// Creates a new embedding generator
    /// 
    /// Downloads the model from HuggingFace Hub if not cached locally.
    /// Uses CPU with Apple Accelerate framework for optimization.
    /// 
    /// # Arguments
    /// 
    /// * `model_id` - HuggingFace model identifier (default: "BAAI/bge-small-en-v1.5")
    /// 
    /// # Returns
    /// 
    /// Result containing the initialized generator or an error
    pub fn new(model_id: &str) -> Result<Self> {
        let device = Device::Cpu;

        let api = Api::new()?;
        let repo = Repo::new(model_id.to_string(), RepoType::Model);
        let api = api.repo(repo);

        let config_path = api.get("config.json")?;
        let tokenizer_path = api.get("tokenizer.json")?;
        let weights_path = api.get("model.safetensors")?;

        let config: Config = serde_json::from_slice(&fs::read(&config_path)?)?;
        let mut tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(anyhow::Error::msg)?;
        tokenizer
            .with_padding(None)
            .with_truncation(None)
            .map_err(anyhow::Error::msg)?;

        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path], DTYPE, &device)?
        };

        let model = BertModel::load(vb, &config)?;

        Ok(Self {
            model,
            tokenizer,
            device,
        })
    }

    /// Generates an embedding vector for the given text
    /// 
    /// Automatically truncates text to 512 tokens (model's maximum).
    /// Returns a 384-dimensional vector suitable for semantic search.
    /// 
    /// # Arguments
    /// 
    /// * `text` - Input text to embed
    /// 
    /// # Returns
    /// 
    /// Result containing a Vec<f32> of length 384 or an error
    pub fn encode(&self, text: &str) -> Result<Vec<f32>> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(anyhow::Error::msg)?;

        let mut ids = encoding.get_ids().to_vec();
        let mut token_type_ids = encoding.get_type_ids().to_vec();
        
        // Truncate to max 512 tokens (model's max length)
        const MAX_LENGTH: usize = 512;
        if ids.len() > MAX_LENGTH {
            ids.truncate(MAX_LENGTH);
            token_type_ids.truncate(MAX_LENGTH);
        }

        let ids_tensor = Tensor::new(&ids[..], &self.device)?.unsqueeze(0)?;
        let token_type_ids_tensor = Tensor::new(&token_type_ids[..], &self.device)?.unsqueeze(0)?;

        let output = self.model.forward(&ids_tensor, &token_type_ids_tensor, None)?;

        // Mean pooling over sequence dimension
        let embeddings_tensor = output.mean(1)?;
        let embedding = embeddings_tensor.squeeze(0)?.to_vec1::<f32>()?;

        Ok(embedding)
    }
}
