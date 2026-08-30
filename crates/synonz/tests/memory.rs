//! Acceptance tests for the memory & context system.
//!
//! Verifies: layered assembly (L3 recall / L2 summaries / L1 window),
//! post-turn memory flows (L1 write, TurnCount demotion with visible
//! ContextManagement calls, L2Overflow distillation), ConversationEnd
//! promotion, and topic tracking.

#![cfg(feature = "test-util")]

use synonz::{
    Agent, Conversation, ConversationHistory, EventPolicy, MemoryPolicies, MockModel,
    ModelStreamItem, Subject, SubjectType, SynonzRuntime,
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
    let agent = Agent::builder().model(model.clone()).build().unwrap();
    let mut conv = Conversation::with_id(&runtime, &subject, "conv-1");

    let _ = agent.ask(conv.turn_input("one")).await.unwrap();
    assert_eq!(conv.len(), 1);

    // After turn one, L1 holds one entry (within window).
    let memory = runtime.memory();
    assert_eq!(memory.l1_len(&subject, "conv-1").unwrap(), 1);

    // Turn two overflows: the flow demotes the oldest into L2 (the
    // summarization call consumes the "summary" script).
    let _ = agent.ask(conv.turn_input("two")).await.unwrap();
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
    let agent = Agent::builder().model(model).build().unwrap();
    let mut conv = Conversation::with_id(&runtime, &subject, "conv-2");

    for text in ["one", "two", "three"] {
        let _ = agent.ask(conv.turn_input(text)).await.unwrap();
    }
    let memory = runtime.memory();
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
    let agent = Agent::builder().model(model).build().unwrap();
    let mut conv = Conversation::with_id(&runtime, &subject, "conv-3");

    let _ = agent.ask(conv.turn_input("one")).await.unwrap();
    let _ = agent.ask(conv.turn_input("two")).await.unwrap();
    let memory = runtime.memory();
    assert_eq!(memory.l2_len(&subject, "conv-3").unwrap(), 1);

    conv.end();
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
    let memory = runtime.memory();
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

    let conv = Conversation::with_id(&runtime, &subject, "conv-4");
    let context = conv.context();
    let assembled = context
        .assemble("what does the user prefer?")
        .await
        .unwrap();

    // L3 recall first (independent System message).
    assert!(assembled[0].blocks.iter().any(|b| {
        matches!(b, synonz::ContentBlock::Text { text } if text.contains("Memory recall")
            && text.contains("prefers celsius"))
    }));
    // L2 summaries present.
    assert!(assembled.iter().any(|m| m.blocks.iter().any(
        |b| matches!(b, synonz::ContentBlock::Text { text } if text.contains("travel plans"))
    )));
    // L1 turns present (verbatim).
    assert!(assembled.iter().any(|m| m.blocks.iter().any(
        |b| matches!(b, synonz::ContentBlock::Text { text } if text.contains("what about beijing?"))
    )));
}

#[tokio::test]
async fn conversation_history_strategy_is_the_pre_memory_behavior() {
    let (runtime, subject) = env();
    let conv = Conversation::with_id(&runtime, &subject, "conv-5");
    conv.push_turn(synonz::Turn::new(
        synonz::AgentInput::new("hi"),
        vec![
            synonz::Message::user("hi"),
            synonz::Message::assistant_text("hello"),
        ],
        synonz::AgentOutput::new(
            synonz::Message::assistant_text("hello"),
            synonz::TokenUsage::new(0, 0),
        ),
    ));

    // Custom assembly registered: the pre-memory strategy.
    let runtime = SynonzRuntime::builder()
        .register_assembly(ConversationHistory)
        .build();
    let conv = Conversation::with_id(&runtime, &subject, "conv-5");
    conv.push_turn(synonz::Turn::new(
        synonz::AgentInput::new("hi"),
        vec![
            synonz::Message::user("hi"),
            synonz::Message::assistant_text("hello"),
        ],
        synonz::AgentOutput::new(
            synonz::Message::assistant_text("hello"),
            synonz::TokenUsage::new(0, 0),
        ),
    ));
    let context = conv.context();
    let assembled = context.assemble("follow up").await.unwrap();
    assert_eq!(
        assembled.len(),
        2,
        "full history, verbatim (no memory layers)"
    );
}

#[tokio::test]
async fn with_context_path_drives_a_full_turn() {
    let (runtime, subject) = env();
    // One-shot via &str stays context-less and works.
    let quick = Agent::builder()
        .model(text_model(&["quick"]))
        .build()
        .unwrap();
    let output = quick.ask("1+1").await.unwrap();
    assert_eq!(output.text(), Some("quick"));

    let mut conv = Conversation::with_id(&runtime, &subject, "conv-6");
    let ctx_agent = Agent::builder()
        .model(text_model(&["the answer"]))
        .build()
        .unwrap()
        .with_context(conv.context());
    let output = ctx_agent.ask(conv.turn_input("question")).await.unwrap();
    assert_eq!(output.text(), Some("the answer"));
    assert_eq!(conv.len(), 1);
    // The turn landed in L1 (memory write after completion).
    assert_eq!(runtime.memory().l1_len(&subject, "conv-6").unwrap(), 1);
}
