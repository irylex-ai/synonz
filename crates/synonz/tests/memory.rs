//! Acceptance tests for the memory & context system (0.2.0 form).
//!
//! Verifies: layered assembly (L3 recall / L2 summaries / L1 window),
//! post-turn memory flows (L1 write, TurnCount demotion with visible
//! ContextManagement calls, L2Overflow distillation), ConversationEnd
//! promotion, topic tracking, custom strategy registration, and the
//! never-silent memory-failure rule.

#![cfg(feature = "test-util")]

use synonz::{
    Agent, AssemblyOutput, ContextAssembly, Conversation, EventPolicy, MemoryFlowStage,
    MemoryPolicies, MockModel, ModelStreamItem, Subject, SubjectType, SynonzRuntime,
};

fn env() -> (SynonzRuntime, Subject) {
    (
        SynonzRuntime::builder().build(),
        Subject::of(SubjectType::User, "test-user"),
    )
}

fn text_model(replies: &[&str]) -> MockModel {
    MockModel::new(
        replies
            .iter()
            .map(|text| {
                vec![ModelStreamItem::Finish {
                    message: synonz::Message::assistant_text(*text),
                    usage: synonz::TokenUsage::new(1, 1),
                }]
            })
            .collect(),
    )
}

#[tokio::test]
async fn post_turn_flow_writes_l1_and_demotes_on_turn_count() {
    let (_unused, subject) = env();
    // Small window: overflow after the first turn, so the summarization
    // (ContextManagement) fires and L2 receives a block.
    let runtime = SynonzRuntime::builder()
        .memory_policies(MemoryPolicies::new(1, 4))
        .build();
    let model = text_model(&["answer one", "summary", "answer two"]);
    let mut conv = Conversation::with_id(&subject, "conv-1");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model.clone())
        .build()
        .unwrap();

    let _ = agent.run(conv.turn_input("one")).await.unwrap();
    assert_eq!(conv.len(), 1);

    // After turn one, L1 holds one entry (within window).
    let memory = runtime.memory_store();
    assert_eq!(memory.l1_len(&subject, "conv-1").unwrap(), 1);

    // Turn two overflows: the flow demotes the oldest into L2 (the
    // summarization call consumes the "summary" script).
    let _ = agent.run(conv.turn_input("two")).await.unwrap();
    assert_eq!(
        memory.l1_len(&subject, "conv-1").unwrap(),
        1,
        "window enforced"
    );
    assert_eq!(
        memory.l2_len(&subject, "conv-1").unwrap(),
        1,
        "demoted into L2"
    );
}

#[tokio::test]
async fn l2_overflow_distills_into_l3() {
    let (_unused, subject) = env();
    let runtime = SynonzRuntime::builder()
        .memory_policies(MemoryPolicies::new(1, 1)) // any second summary block distills into L3
        .build();
    // Scripts: turn answers + one summarization per demotion.
    let model = text_model(&["a1", "sum1", "a2", "sum2", "a3"]);
    let mut conv = Conversation::with_id(&subject, "conv-2");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model)
        .build()
        .unwrap();

    for text in ["one", "two", "three"] {
        let _ = agent.run(conv.turn_input(text)).await.unwrap();
    }
    let memory = runtime.memory_store();
    assert_eq!(memory.l1_len(&subject, "conv-2").unwrap(), 1);
    // L2 capped at 1; the rest distilled into L3.
    assert_eq!(memory.l2_len(&subject, "conv-2").unwrap(), 1);
    assert!(memory.l3_len(&subject).unwrap() >= 1, "distilled into L3");
}

#[tokio::test]
async fn conversation_end_promotes_l2_into_l3() {
    let (_unused, subject) = env();
    let runtime = SynonzRuntime::builder()
        .memory_policies(MemoryPolicies::new(1, 4).with_extra([EventPolicy::ConversationEnd]))
        .build();
    let model = text_model(&["a1", "sum1", "a2"]);
    let mut conv = Conversation::with_id(&subject, "conv-3");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(model)
        .build()
        .unwrap();

    let _ = agent.run(conv.turn_input("one")).await.unwrap();
    let _ = agent.run(conv.turn_input("two")).await.unwrap();
    let memory = runtime.memory_store();
    assert_eq!(memory.l2_len(&subject, "conv-3").unwrap(), 1);

    conv.end(&runtime);
    assert_eq!(
        memory.l2_len(&subject, "conv-3").unwrap(),
        0,
        "promoted away"
    );
    assert!(memory.l3_len(&subject).unwrap() >= 1);
}

#[tokio::test]
async fn layered_assembly_reads_memory_layers() {
    let (runtime, subject) = env();
    // Seed L1/L2/L3 directly through the memory contract.
    let memory = runtime.memory_store();
    memory
        .l1_append(
            &subject,
            "conv-4",
            &"weather".to_string(),
            vec![
                synonz::Message::user("what about beijing?"),
                synonz::Message::assistant_text("sunny"),
            ],
        )
        .unwrap();
    memory
        .l2_append(
            &subject,
            synonz::SummaryBlock::new("conv-4", "earlier we discussed travel plans", 0),
        )
        .unwrap();
    memory
        .l3_upsert(
            &subject,
            synonz::KnowledgeFragment::new(
                synonz::FragmentIdentity {
                    subject_id: subject.to_string(),
                    conversation_id: "old-conv".into(),
                    topic: "weather".into(),
                },
                "the user prefers celsius",
            ),
        )
        .unwrap();

    let conv = Conversation::with_id(&subject, "conv-4");
    let context = conv.context(&runtime);
    let assembled = context.assemble("what does the user prefer?").await;
    assert!(assembled.failures.is_empty(), "clean reads, no degradation");

    // L3 recall first (independent System message).
    assert!(assembled.messages[0].blocks.iter().any(|b| {
        matches!(b, synonz::ContentBlock::Text { text } if text.contains("Memory recall")
            && text.contains("prefers celsius"))
    }));
    // L2 summaries present.
    assert!(assembled.messages.iter().any(|m| m.blocks.iter().any(
        |b| matches!(b, synonz::ContentBlock::Text { text } if text.contains("travel plans"))
    )));
    // L1 turns present (verbatim).
    assert!(assembled.messages.iter().any(|m| m.blocks.iter().any(
        |b| matches!(b, synonz::ContentBlock::Text { text } if text.contains("what about beijing?"))
    )));
}

/// A custom strategy: the plugin contract survives the narrowing — it
/// reads memory through the request and composes its own background.
struct PrependStrategy;

impl ContextAssembly for PrependStrategy {
    fn assemble<'a>(
        &'a self,
        request: synonz::AssemblyRequest<'a>,
    ) -> synonz::BoxFuture<'a, Result<AssemblyOutput, synonz::AssemblyError>> {
        Box::pin(async move {
            let mut output = AssemblyOutput::default();
            output
                .messages
                .push(synonz::Message::system("custom strategy was here"));
            for entry in request
                .memory
                .l1_window(request.subject, request.conversation_id)
                .unwrap_or_default()
            {
                output.messages.extend(entry.messages);
            }
            Ok(output)
        })
    }
}

#[tokio::test]
async fn custom_strategy_registration_drives_assembly() {
    let (_first, subject) = env();
    let runtime = SynonzRuntime::builder()
        .context_assembly(PrependStrategy)
        .build();
    // Seed L1 through the memory contract (the strategy reads memory).
    runtime
        .memory_store()
        .l1_append(
            &subject,
            "conv-5",
            &"chat".to_string(),
            vec![synonz::Message::user("hi")],
        )
        .unwrap();

    let conv = Conversation::with_id(&subject, "conv-5");
    let context = conv.context(&runtime);
    let assembled = context.assemble("follow up").await;
    assert!(assembled.failures.is_empty());
    assert_eq!(assembled.messages.len(), 2, "marker + seeded L1, verbatim");
    assert!(assembled.messages[0].blocks.iter().any(
        |b| matches!(b, synonz::ContentBlock::Text { text } if text == "custom strategy was here")
    ));
}

/// A memory store whose reads fail — the degradation must be visible in
/// the assembly output, never silent.
struct BrokenMemory;

impl synonz::MemoryStore for BrokenMemory {
    fn l1_append(
        &self,
        _: &Subject,
        _: &str,
        _: &String,
        _: Vec<synonz::Message>,
    ) -> Result<(), synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn l1_window(
        &self,
        _: &Subject,
        _: &str,
    ) -> Result<Vec<synonz::L1Entry>, synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn l1_len(&self, _: &Subject, _: &str) -> Result<usize, synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn l1_pop_oldest(
        &self,
        _: &Subject,
        _: &str,
        _: usize,
    ) -> Result<Vec<synonz::L1Entry>, synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn l2_append(
        &self,
        _: &Subject,
        synonz::SummaryBlock { .. }: synonz::SummaryBlock,
    ) -> Result<(), synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn l2_read(
        &self,
        _: &Subject,
        _: &str,
    ) -> Result<Vec<synonz::SummaryBlock>, synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn l2_len(&self, _: &Subject, _: &str) -> Result<usize, synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn l2_pop_oldest(
        &self,
        _: &Subject,
        _: &str,
        _: usize,
    ) -> Result<Vec<synonz::SummaryBlock>, synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn l3_upsert(
        &self,
        _: &Subject,
        _: synonz::KnowledgeFragment,
    ) -> Result<(), synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn l3_retrieve(
        &self,
        _: &Subject,
        _: &str,
        _: &String,
        _: usize,
    ) -> Result<Vec<synonz::KnowledgeFragment>, synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
    fn l3_len(&self, _: &Subject) -> Result<usize, synonz::MemoryStoreError> {
        Err(synonz::MemoryStoreError::Storage("down".into()))
    }
}

#[tokio::test]
async fn assembly_memory_failures_are_visible_not_silent() {
    // Override the default store with the failing one: the degradation
    // must be visible in the assembly output, never silent.
    let runtime = SynonzRuntime::builder().memory_store(BrokenMemory).build();
    let conv = Conversation::with_id(&Subject::of(SubjectType::User, "u"), "conv-6");

    let context = conv.context(&runtime);
    let assembled = context.assemble("anything").await;

    // Every layer failed and every failure is reported — no silent
    // "no memory" degradation.
    let stages: Vec<&MemoryFlowStage> = assembled
        .failures
        .iter()
        .map(|failure| &failure.stage)
        .collect();
    assert!(stages.contains(&&MemoryFlowStage::AssembleRead));
    assert_eq!(stages.len(), 3, "l1, l2, l3 all report");
    assert!(assembled.messages.is_empty());
}

#[tokio::test]
async fn background_engine_drives_a_full_turn() {
    let (runtime, subject) = env();
    let mut conv = Conversation::with_id(&subject, "conv-7");
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(text_model(&["the answer"]))
        .build()
        .unwrap();

    // The context is derived from the conversation at execution time; the
    // completed turn lands in the truth archive AND in L1.
    let output = agent.run(conv.turn_input("question")).await.unwrap();
    assert_eq!(output.text(), Some("the answer"));
    assert_eq!(conv.len(), 1);
    assert_eq!(
        runtime.memory_store().l1_len(&subject, "conv-7").unwrap(),
        1
    );
}
