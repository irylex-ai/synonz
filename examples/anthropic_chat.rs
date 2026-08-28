//! `anthropic_chat`: a real Anthropic chat.
//!
//! Requires `SYNONZ_ANTHROPIC_API_KEY` (optionally `SYNONZ_ANTHROPIC_BASE_URL`
//! and `SYNONZ_ANTHROPIC_MODEL`). Without the key this example prints a
//! hint and exits.
//!
//! Run: `cargo run -p synonz-examples --bin anthropic_chat`

use synonz_anthropic::Client;

#[tokio::main]
async fn main() {
    let Ok(api_key) = std::env::var("SYNONZ_ANTHROPIC_API_KEY") else {
        eprintln!("set SYNONZ_ANTHROPIC_API_KEY to run this example");
        return;
    };
    let base_url = std::env::var("SYNONZ_ANTHROPIC_BASE_URL")
        .unwrap_or_else(|_| synonz_anthropic::DEFAULT_BASE_URL.into());
    let model_name =
        std::env::var("SYNONZ_ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-sonnet-4-5".into());

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
