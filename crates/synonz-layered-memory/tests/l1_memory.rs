//! L1 working memory: the store's window behavior and the turn archiving.

mod common;

use synonz::{Agent, MemoryScope};
use synonz_layered_memory::{InProcessL1MemoryStore, L1MemoryEntry, L1MemoryStore};

#[test]
fn the_store_keeps_the_window_and_evicts_the_oldest_turns() {
    let store = InProcessL1MemoryStore::new(3);
    let scope = MemoryScope::new("conversation:c1");
    for index in 0..5 {
        store
            .append(L1MemoryEntry::new(
                scope.clone(),
                "topic",
                format!("question {index}"),
                format!("answer {index}"),
                index as u64,
            ))
            .unwrap();
    }

    let recent = store.recent(&scope, 10).unwrap();
    assert_eq!(recent.len(), 3, "one entry per turn, capacity 3");
    assert_eq!(recent[0].input, "question 2");
    assert_eq!(recent[2].response, "answer 4");
    assert_eq!(store.count(&scope).unwrap(), 3);
    assert_eq!(
        store
            .count(&MemoryScope::new("conversation:other"))
            .unwrap(),
        0
    );

    assert!(store.remove(&scope, &recent[0].id).unwrap());
    assert!(!store.remove(&scope, "missing").unwrap());
    assert_eq!(store.count(&scope).unwrap(), 2);

    store.clear(&scope).unwrap();
    assert_eq!(store.count(&scope).unwrap(), 0);
}

#[tokio::test]
async fn completed_turns_are_archived_with_input_and_final_answer() {
    let fixture = common::fixture(common::Parts::default());
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(common::mock(&["the answer"]))
        .build()
        .unwrap();
    let mut conversation = fixture.conversation();

    agent
        .run(conversation.turn_input("the question"))
        .await
        .unwrap();

    let scope = fixture.conversation_scope(&conversation);
    let entries = fixture.l1.recent(&scope, 10).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].input, "the question");
    assert_eq!(entries[0].response, "the answer");
    assert_eq!(entries[0].scope, scope);
}

#[tokio::test]
async fn failed_turns_never_reach_working_memory() {
    struct FailingModel;

    impl synonz::Model for FailingModel {
        fn stream(
            &self,
            _request: synonz::ModelRequest,
        ) -> synonz::BoxFuture<'_, Result<synonz::ModelStream, synonz::ModelError>> {
            Box::pin(async {
                Err(synonz::ModelError::Transport {
                    message: "down".into(),
                })
            })
        }
    }

    let fixture = common::fixture(common::Parts::default());
    let agent = Agent::builder()
        .runtime(&fixture.runtime)
        .model(FailingModel)
        .build()
        .unwrap();
    let mut conversation = fixture.conversation();

    let _ = agent.run(conversation.turn_input("fails")).await;

    let scope = fixture.conversation_scope(&conversation);
    assert_eq!(fixture.l1.count(&scope).unwrap(), 0);
    assert_eq!(conversation.turns().len(), 1, "the truth archive keeps it");
}
