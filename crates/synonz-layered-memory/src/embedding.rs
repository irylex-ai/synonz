//! The embedding port and its bundled deterministic implementation.

use futures::future::BoxFuture;
use synonz::MemoryFailure;

/// The component-side embedding port: vectorizes text for recall scoring,
/// alias matching, and anchoring. Implementations are component
/// configuration (a local model, a hosted endpoint, ...); failures surface
/// through the framework like any other component failure.
pub trait Embedding: Send + Sync + 'static {
    /// Embeds one text (the asynchronous path used by the memory flows).
    fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, MemoryFailure>>;

    /// The optional synchronous fast path, used by the synchronous
    /// management verbs so an edit refreshes its recall vector
    /// immediately.
    ///
    /// Local implementations provide it; implementations whose
    /// vectorization is inherently asynchronous keep the default (`None`),
    /// and the component defers the refresh to its next background point
    /// (the window filling, a topic shift, or conversation end).
    fn embed_inline(&self, _text: &str) -> Option<Result<Vec<f32>, MemoryFailure>> {
        None
    }
}

/// The bundled deterministic embedding: a hashed bag-of-tokens vector,
/// L2-normalized. Process-local and dependency-free — the development
/// default, not a semantic model.
pub struct HashEmbedding {
    dimensions: usize,
}

impl HashEmbedding {
    /// Creates the embedding with a fixed dimensionality.
    pub fn new(dimensions: usize) -> Self {
        Self { dimensions }
    }
}

impl Default for HashEmbedding {
    fn default() -> Self {
        Self::new(256)
    }
}

impl Embedding for HashEmbedding {
    fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, MemoryFailure>> {
        let vector = self.vector(text);
        Box::pin(async move { Ok(vector) })
    }

    fn embed_inline(&self, text: &str) -> Option<Result<Vec<f32>, MemoryFailure>> {
        Some(Ok(self.vector(text)))
    }
}

impl HashEmbedding {
    /// The deterministic vector of one text.
    fn vector(&self, text: &str) -> Vec<f32> {
        let mut vector = vec![0.0f32; self.dimensions];
        for token in tokens(text) {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&token, &mut hasher);
            let index = (std::hash::Hasher::finish(&hasher) as usize) % self.dimensions;
            vector[index] += 1.0;
        }
        let norm: f32 = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if norm > 0.0 {
            for value in &mut vector {
                *value /= norm;
            }
        }
        vector
    }
}

/// Splits text into embedding tokens: word-ish runs (lowercased) plus, for
/// non-ASCII runs, their individual characters (so CJK text still shares
/// signal).
fn tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, tokens: &mut Vec<String>| {
        if !current.is_empty() {
            tokens.push(std::mem::take(current));
        }
    };
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else {
            flush(&mut current, &mut tokens);
        }
    }
    flush(&mut current, &mut tokens);
    tokens
        .into_iter()
        .flat_map(|token| {
            if token.is_ascii() {
                vec![token]
            } else {
                let chars: Vec<String> = token.chars().map(|ch| ch.to_string()).collect();
                std::iter::once(token).chain(chars).collect()
            }
        })
        .collect()
}
