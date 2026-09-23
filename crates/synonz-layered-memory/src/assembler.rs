//! The read phase: three-layer recall, the unified score, dedupe, the
//! cold-start gate, and the context rewrite into the model-visible frame.

use std::sync::Arc;

use futures::future::BoxFuture;
use synonz::{
    MemoryContextAssembleInput, MemoryContextAssembleOutput, MemoryContextAssembler, MemoryFailure,
    Message, Model, ModelRequest, complete,
};

use crate::config::{LayeredMemoryConfig, MemoryScopeResolver};
use crate::embedding::Embedding;
use crate::l1_memory::{L1Memory, L1MemoryEntry};
use crate::l2_memory::L2Memory;
use crate::l3_memory::{L3Memory, entity_text};
use crate::utils::{conversation_scope, cosine, now_epoch};

/// One recall candidate (private carrier for scoring, dedupe and ranking).
struct Candidate {
    text: String,
    vector: Vec<f32>,
    score: f32,
}

/// The context rewrite strategy: composes the model-visible enhanced input
/// from the recalled items and the current query.
///
/// The strategy folds the recalled context into one self-contained request
/// (keeping the user's intent and language); it must not answer.
pub trait LayeredMemoryContextRewriter: Send + Sync + 'static {
    /// Produces the enhanced message.
    fn rewrite<'a>(
        &'a self,
        input: LayeredMemoryContextRewriteInput<'a>,
    ) -> BoxFuture<'a, Result<String, MemoryFailure>>;
}

/// The rewriter's material: the query and the recalled items (rendered).
#[non_exhaustive]
pub struct LayeredMemoryContextRewriteInput<'a> {
    /// The query text (`rewritten_input` when present, the original input
    /// otherwise).
    pub input: &'a str,
    /// The recalled items, already ranked and deduped.
    pub recalled: &'a [String],
    /// The narrated model handle.
    pub model: Arc<dyn Model>,
}

impl<'a> LayeredMemoryContextRewriteInput<'a> {
    /// Assembles the payload.
    pub fn new(input: &'a str, recalled: &'a [String], model: Arc<dyn Model>) -> Self {
        Self {
            input,
            recalled,
            model,
        }
    }
}

/// The prompt-driven default rewriter.
pub struct LayeredMemoryPromptContextRewriter;

impl LayeredMemoryContextRewriter for LayeredMemoryPromptContextRewriter {
    fn rewrite<'a>(
        &'a self,
        input: LayeredMemoryContextRewriteInput<'a>,
    ) -> BoxFuture<'a, Result<String, MemoryFailure>> {
        Box::pin(async move {
            let mut recalled = String::new();
            for item in input.recalled {
                recalled.push_str("- ");
                recalled.push_str(item);
                recalled.push('\n');
            }
            let prompt = format!(
                "Rewrite the user's message into a self-contained request, folding in the \
                 relevant memory context below. Keep the user's intent and language; do not \
                 answer the request.\n\nMemory context:\n{recalled}\nUser message:\n{message}\n\n\
                 Reply with the rewritten message only.",
                message = input.input
            );
            let request = ModelRequest::new(vec![Message::user(prompt)], Vec::new());
            let (message, _usage) = complete(&*input.model, request)
                .await
                .map_err(|error| MemoryFailure::new("rewrite", error.to_string()))?;
            let text = message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    synonz::ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>();
            if text.trim().is_empty() {
                return Err(MemoryFailure::new(
                    "rewrite",
                    "the model reply carried no rewritten input",
                ));
            }
            Ok(text)
        })
    }
}

/// The component's assembler: the three-layer recall (working memory, event
/// summaries, long-term graph), the unified score, dedupe, the cold-start
/// gate, and the context rewrite into the complete model-visible frame.
pub(crate) struct LayeredMemoryContextAssembler {
    l1: Arc<L1Memory>,
    l2: Arc<L2Memory>,
    l3: Arc<L3Memory>,
    embedding: Arc<dyn Embedding>,
    rewriter: Arc<dyn LayeredMemoryContextRewriter>,
    scope_resolver: Arc<dyn MemoryScopeResolver>,
    config: Arc<LayeredMemoryConfig>,
}

impl LayeredMemoryContextAssembler {
    /// Builds the assembler over its collaborators.
    pub(crate) fn new(
        l1: Arc<L1Memory>,
        l2: Arc<L2Memory>,
        l3: Arc<L3Memory>,
        embedding: Arc<dyn Embedding>,
        rewriter: Arc<dyn LayeredMemoryContextRewriter>,
        scope_resolver: Arc<dyn MemoryScopeResolver>,
        config: Arc<LayeredMemoryConfig>,
    ) -> Self {
        Self {
            l1,
            l2,
            l3,
            embedding,
            rewriter,
            scope_resolver,
            config,
        }
    }
}

impl MemoryContextAssembler for LayeredMemoryContextAssembler {
    fn assemble<'a>(
        &'a self,
        input: MemoryContextAssembleInput<'a>,
    ) -> BoxFuture<'a, MemoryContextAssembleOutput> {
        Box::pin(async move {
            let mut failures = Vec::new();
            let params = &self.config;
            let query = input.rewritten_input.unwrap_or(input.input);
            let partition = conversation_scope(input.conversation_id);
            let query_vector = match self.embedding.embed(query).await {
                Ok(vector) => Some(vector),
                Err(failure) => {
                    failures.push(failure);
                    None
                }
            };

            let mut candidates: Vec<Candidate> = Vec::new();

            // L1: the conversation's turns.
            let l1_entries = match self.l1.entries(&partition, params.l1_turns) {
                Ok(entries) => entries,
                Err(error) => {
                    failures.push(MemoryFailure::new("recall", format!("l1: {error}")));
                    Vec::new()
                }
            };
            for turn in &l1_entries {
                self.push_l1_candidate(&mut candidates, turn, query_vector.as_deref(), input.topic)
                    .await
                    .unwrap_or_else(|failure| failures.push(failure));
            }

            // L2: the conversation's entries.
            let l2_entries = match self.l2.entries(&partition) {
                Ok(entries) => entries,
                Err(error) => {
                    failures.push(MemoryFailure::new("recall", format!("l2: {error}")));
                    Vec::new()
                }
            };
            for entry in &l2_entries {
                let semantic = query_vector
                    .as_deref()
                    .map(|query| cosine(query, &entry.embedding).clamp(0.0, 1.0))
                    .unwrap_or(0.0);
                let mut score =
                    score_components(semantic, entry.updated_at, entry.importance, params);
                score *= params.recall_user_weight;
                if boosted(&entry.topic, input.topic, params) {
                    score *= params.recall_source_boost;
                }
                candidates.push(Candidate {
                    text: entry.content.clone(),
                    vector: entry.embedding.clone(),
                    score,
                });
            }

            // L3: anchors first, then a bounded walk — gated while working
            // memory is empty (cold start).
            let working_empty = l1_entries.is_empty() && l2_entries.is_empty();
            if !working_empty && let Some(query_vector) = query_vector.as_deref() {
                for scope in self
                    .scope_resolver
                    .resolve(input.subject, input.conversation_id)
                {
                    let anchors =
                        match self
                            .l3
                            .anchors(&scope, query_vector, params.l3_anchor_top_n)
                        {
                            Ok(anchors) => anchors,
                            Err(error) => {
                                failures.push(MemoryFailure::new("recall", format!("l3: {error}")));
                                continue;
                            }
                        };
                    for (name, similarity) in anchors {
                        if similarity < params.l3_anchor_similarity {
                            continue;
                        }
                        let Ok(Some(entity)) = self.l3.entity(&scope, &name) else {
                            continue;
                        };
                        let text = entity_text(&entity);
                        let vector = match self.embedding.embed(&text).await {
                            Ok(vector) => vector,
                            Err(failure) => {
                                failures.push(failure);
                                Vec::new()
                            }
                        };
                        candidates.push(Candidate {
                            text,
                            vector,
                            score: score_components(
                                similarity,
                                entity.updated_at,
                                params.recall_default_importance,
                                params,
                            ),
                        });
                        let mut frontier = vec![name.clone()];
                        let mut visited = vec![name.clone()];
                        for hop in 1..=params.l3_max_hops {
                            let mut next = Vec::new();
                            for node in &frontier {
                                let Ok(edges) = self.l3.relations_of(&scope, node) else {
                                    continue;
                                };
                                for edge in edges {
                                    let other = if &edge.from == node {
                                        edge.to.clone()
                                    } else {
                                        edge.from.clone()
                                    };
                                    if visited.contains(&other) {
                                        continue;
                                    }
                                    visited.push(other.clone());
                                    next.push(other.clone());
                                    let Ok(Some(entity)) = self.l3.entity(&scope, &other) else {
                                        continue;
                                    };
                                    let text = entity_text(&entity);
                                    let vector = match self.embedding.embed(&text).await {
                                        Ok(vector) => vector,
                                        Err(failure) => {
                                            failures.push(failure);
                                            Vec::new()
                                        }
                                    };
                                    candidates.push(Candidate {
                                        text,
                                        vector,
                                        score: score_components(
                                            similarity * params.l3_hop_decay.powi(hop as i32),
                                            entity.updated_at,
                                            params.recall_default_importance,
                                            params,
                                        ),
                                    });
                                }
                            }
                            frontier = next;
                            if frontier.is_empty() {
                                break;
                            }
                        }
                    }
                }
            }

            // Dedupe, rank, take the top N.
            let mut ranked = dedupe(candidates, params);
            ranked.truncate(params.recall_top_n);

            let messages = if ranked.is_empty() {
                vec![Message::user(query)]
            } else {
                let recalled: Vec<String> = ranked.iter().map(|item| item.text.clone()).collect();
                let rewrite = self
                    .rewriter
                    .rewrite(LayeredMemoryContextRewriteInput::new(
                        query,
                        &recalled,
                        Arc::clone(&input.model),
                    ))
                    .await;
                match rewrite {
                    Ok(enhanced) => vec![Message::user(enhanced)],
                    Err(failure) => {
                        failures.push(failure);
                        vec![Message::user(query)]
                    }
                }
            };

            MemoryContextAssembleOutput::new(messages, failures)
        })
    }
}

impl LayeredMemoryContextAssembler {
    async fn push_l1_candidate(
        &self,
        candidates: &mut Vec<Candidate>,
        turn: &L1MemoryEntry,
        query_vector: Option<&[f32]>,
        topic: &str,
    ) -> Result<(), MemoryFailure> {
        let params = &self.config;
        let input_vector = self.embedding.embed(&turn.input).await?;
        let response_vector = if turn.response.is_empty() {
            Vec::new()
        } else {
            self.embedding.embed(&turn.response).await?
        };
        let (semantic, track_weight, vector) = match query_vector {
            None => (0.0, params.recall_user_weight, input_vector),
            Some(query) => {
                let sim_input = cosine(query, &input_vector).clamp(0.0, 1.0);
                let sim_response = if response_vector.is_empty() {
                    0.0
                } else {
                    cosine(query, &response_vector).clamp(0.0, 1.0)
                };
                if sim_input * params.recall_user_weight
                    >= sim_response * params.recall_agent_weight
                {
                    (sim_input, params.recall_user_weight, input_vector)
                } else {
                    (sim_response, params.recall_agent_weight, response_vector)
                }
            }
        };
        let mut score = score_components(
            semantic,
            turn.created_at,
            params.recall_default_importance,
            params,
        );
        score *= track_weight;
        if boosted(&turn.topic, topic, params) {
            score *= params.recall_source_boost;
        }
        let text = if turn.response.is_empty() {
            format!("用户：{}", turn.input)
        } else {
            format!("用户：{}\n助手：{}", turn.input, turn.response)
        };
        candidates.push(Candidate {
            text,
            vector,
            score,
        });
        Ok(())
    }
}

/// The unified score: `semantic·w + recency·w + importance·w`.
fn score_components(
    semantic: f32,
    updated_at: u64,
    importance: f32,
    params: &LayeredMemoryConfig,
) -> f32 {
    let semantic = semantic.clamp(0.0, 1.0);
    let age = now_epoch().saturating_sub(updated_at);
    let recency = 0.5f32.powf(age as f32 / params.recall_time_half_life.max(1) as f32);
    params.recall_semantic_weight * semantic
        + params.recall_time_weight * recency
        + params.recall_importance_weight * importance.clamp(0.0, 1.0)
}

/// Whether one topic matches the conversation's current topic (the topic
/// boost).
fn boosted(candidate_topic: &str, current_topic: &str, params: &LayeredMemoryConfig) -> bool {
    params.recall_source_boost != 1.0
        && !candidate_topic.is_empty()
        && candidate_topic == current_topic
}

/// Ranks by score and drops duplicates (identical text or near-identical
/// vectors).
fn dedupe(mut items: Vec<Candidate>, params: &LayeredMemoryConfig) -> Vec<Candidate> {
    items.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.text.cmp(&b.text))
    });
    let mut kept: Vec<Candidate> = Vec::with_capacity(items.len());
    for item in items {
        let duplicate = kept.iter().any(|existing| {
            existing.text == item.text
                || (!item.vector.is_empty()
                    && !existing.vector.is_empty()
                    && cosine(&existing.vector, &item.vector) >= params.recall_dedupe_similarity)
        });
        if !duplicate {
            kept.push(item);
        }
    }
    kept
}
