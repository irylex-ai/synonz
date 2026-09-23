//! The component's configuration: capacities, recall weights, thresholds,
//! the long-term vocabulary, and the scope resolver.

use synonz::{MemoryScope, Subject};

use crate::l3_memory::L3MemorySchema;

/// Long-term partition resolution: the component asks, at use time (when
/// the conversation exists), which long-term partitions a conversation's
/// knowledge belongs to. The application can consult its own project store,
/// so nothing depends on the provider's construction time.
pub trait MemoryScopeResolver: Send + Sync + 'static {
    /// The long-term partitions of one conversation (empty = no long-term
    /// memory for it).
    fn resolve(&self, subject: &Subject, conversation_id: &str) -> Vec<MemoryScope>;
}

/// The default resolver: one per-subject partition (`user:<subject>`).
pub(crate) struct SubjectScopeResolver;

impl MemoryScopeResolver for SubjectScopeResolver {
    fn resolve(&self, subject: &Subject, _conversation_id: &str) -> Vec<MemoryScope> {
        vec![MemoryScope::new(format!("user:{subject}"))]
    }
}

/// The component's tunables (flat, domain-prefixed: `l1_` / `l2_` / `l3_` /
/// `recall_` / `topic_`).
#[derive(Debug, Clone, PartialEq)]
pub struct LayeredMemoryConfig {
    /// How many turns of working memory stay verbatim.
    pub l1_turns: usize,
    /// How many entries a conversation keeps.
    pub l2_entries: usize,
    /// How many previous contents an entry keeps.
    pub l2_versions: usize,
    /// The maximum length of an entry's content, in characters.
    pub l2_content_chars: usize,
    /// The long-term vocabulary and its fallbacks.
    pub l3_schema: L3MemorySchema,
    /// The embedding similarity at or above which an alias candidate
    /// triggers the LLM judgment.
    pub l3_alias_similarity: f32,
    /// How many known entities the extraction context may carry.
    pub l3_extraction_entities: usize,
    /// How many vector-search anchors seed the graph walk.
    pub l3_anchor_top_n: usize,
    /// The similarity at or above which an entity becomes an anchor.
    pub l3_anchor_similarity: f32,
    /// The maximum graph walk depth (1–2 hops).
    pub l3_max_hops: usize,
    /// The score decay per graph hop.
    pub l3_hop_decay: f32,
    /// How many recalled items the assembled context keeps.
    pub recall_top_n: usize,
    /// The semantic-similarity weight.
    pub recall_semantic_weight: f32,
    /// The recency weight.
    pub recall_time_weight: f32,
    /// The importance weight.
    pub recall_importance_weight: f32,
    /// The user-track multiplier.
    pub recall_user_weight: f32,
    /// The agent-track multiplier.
    pub recall_agent_weight: f32,
    /// The multiplier for candidates whose topic matches the current topic.
    pub recall_source_boost: f32,
    /// The similarity at or above which two recalled items are duplicates.
    pub recall_dedupe_similarity: f32,
    /// The recency half-life (seconds) used by the time score.
    pub recall_time_half_life: u64,
    /// The neutral importance used where no importance signal exists
    /// (working memory and long-term memory).
    pub recall_default_importance: f32,
    /// The embedding similarity at or above which a new segment keeps the
    /// current topic.
    pub topic_shift_similarity: f32,
}

impl Default for LayeredMemoryConfig {
    fn default() -> Self {
        Self {
            l1_turns: 10,
            l2_entries: 5,
            l2_versions: 5,
            l2_content_chars: 30,
            l3_schema: L3MemorySchema::default(),
            l3_alias_similarity: 0.85,
            l3_extraction_entities: 20,
            l3_anchor_top_n: 3,
            l3_anchor_similarity: 0.5,
            l3_max_hops: 2,
            l3_hop_decay: 0.7,
            recall_top_n: 5,
            recall_semantic_weight: 0.5,
            recall_time_weight: 0.3,
            recall_importance_weight: 0.2,
            recall_user_weight: 1.2,
            recall_agent_weight: 1.0,
            recall_source_boost: 1.2,
            recall_dedupe_similarity: 0.95,
            recall_time_half_life: 7 * 24 * 3600,
            recall_default_importance: 0.65,
            topic_shift_similarity: 0.5,
        }
    }
}
