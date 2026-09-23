//! The write-side preprocessing extension: topic detection by embedding
//! similarity against the current topic, with a light guard for trivial
//! inputs.

use std::sync::Arc;

use futures::future::BoxFuture;
use synonz::{MemoryFailure, Model, TopicDetectInput, TopicDetector, TopicDetectorProvider};

use crate::config::LayeredMemoryConfig;
use crate::embedding::Embedding;
use crate::utils::{cosine, first_segment, is_trivial};

/// The component's topic detector.
pub(crate) struct SimilarityTopicDetector {
    embedding: Arc<dyn Embedding>,
    config: Arc<LayeredMemoryConfig>,
}

impl TopicDetector for SimilarityTopicDetector {
    fn detect<'a>(
        &'a self,
        input: TopicDetectInput<'a>,
    ) -> BoxFuture<'a, Result<Option<String>, MemoryFailure>> {
        Box::pin(async move {
            let candidate = first_segment(input.input);
            let Some(current) = input.topic.filter(|topic| !topic.is_empty()) else {
                // The first topic is an establishment.
                if candidate.trim().is_empty() {
                    return Ok(None);
                }
                return Ok(Some(candidate));
            };
            if is_trivial(input.input) || candidate.trim().is_empty() {
                return Ok(None);
            }
            let current_vector = self.embedding.embed(current).await?;
            let candidate_vector = self.embedding.embed(&candidate).await?;
            if cosine(&current_vector, &candidate_vector) >= self.config.topic_shift_similarity {
                return Ok(None);
            }
            Ok(Some(candidate))
        })
    }
}

/// The component's topic detection provider (register it on an agent). Its
/// model is the component's configured model; when the component has none,
/// the framework falls back to the agent model (this detector itself is
/// embedding-based and makes no model calls).
pub struct SimilarityTopicDetectorProvider {
    embedding: Arc<dyn Embedding>,
    config: Arc<LayeredMemoryConfig>,
    model: Option<Arc<dyn Model>>,
}

impl SimilarityTopicDetectorProvider {
    /// Builds the provider.
    pub(crate) fn new(
        embedding: Arc<dyn Embedding>,
        config: Arc<LayeredMemoryConfig>,
        model: Option<Arc<dyn Model>>,
    ) -> Self {
        Self {
            embedding,
            config,
            model,
        }
    }
}

impl TopicDetectorProvider for SimilarityTopicDetectorProvider {
    fn topic_detector(&self) -> Arc<dyn TopicDetector> {
        Arc::new(SimilarityTopicDetector {
            embedding: Arc::clone(&self.embedding),
            config: Arc::clone(&self.config),
        })
    }

    fn model(&self) -> Option<Arc<dyn Model>> {
        self.model.clone()
    }
}
