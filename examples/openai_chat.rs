//! `openai_chat`: a real OpenAI-compatible chat.
//!
//! Requires `SYNONZ_OPENAI_API_KEY` (optionally `SYNONZ_OPENAI_BASE_URL`
//! and `SYNONZ_OPENAI_MODEL`). Without the key this example prints a hint
//! and exits.
//!
//! Run: `cargo run -p synonz-examples --bin openai_chat`

use synonz_openai::Client;

#[tokio::main]
async fn main() {
    let Ok(api_key) = std::env::var("SYNONZ_OPENAI_API_KEY") else {
        eprintln!("set SYNONZ_OPENAI_API_KEY to run this example");
        return;
    };
    let base_url = std::env::var("SYNONZ_OPENAI_BASE_URL")
        .unwrap_or_else(|_| synonz_openai::DEFAULT_BASE_URL.into());
    let model_name = std::env::var("SYNONZ_OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());

    let agent = synonz::Agent::builder()
        .model(Client::new(base_url, api_key, model_name))
        .system_prompt("reply in one short sentence")
        .build()
        .expect("model is set");

    let mut run = agent.run("what is the weather like in beijing?");
    let mut delta_text = String::new();
    while let Some(event) = run.next().await {
        if let synonz::AgentEvent::Model(synonz::ModelEvent::StreamDelta {
            delta: synonz::ModelDelta::Text { text },
        }) = event
        {
            delta_text.push_str(&text);
            print!("{text}");
        }
    }
    println!();
    assert!(!delta_text.is_empty());
}
