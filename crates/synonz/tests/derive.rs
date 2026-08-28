//! `#[derive(Tool)]` acceptance tests: the derived implementation must be
//! equivalent to a hand-written one, and the whole flow must work from an
//! external crate's perspective (this file only uses the public API).

use std::sync::OnceLock;

use serde_json::{Value, json};
use synonz::{Deserialize, JsonSchema, Tool, ToolContent, ToolContext, ToolError, ToolResult};

// ── The derived tool ──

/// 查询指定城市的当前天气。
#[derive(Tool, Deserialize, JsonSchema)]
struct WeatherDerived {
    /// 城市名，如 "beijing"。
    city: String,
    /// 可选的温度单位，默认摄氏度。
    unit: Option<String>,
}

impl WeatherDerived {
    async fn run(&self) -> Result<ToolResult, ToolError> {
        let unit = self.unit.as_deref().unwrap_or("celsius");
        Ok(ToolResult::Ok {
            content: ToolContent::Text {
                text: format!("{}: 28 {}", self.city, unit),
            },
        })
    }
}

// ── The hand-written equivalent ──

struct WeatherManual;

impl Tool for WeatherManual {
    fn name(&self) -> &str {
        "weather_derived"
    }
    fn description(&self) -> &str {
        "查询指定城市的当前天气。"
    }
    fn parameters_schema(&self) -> &Value {
        static SCHEMA: OnceLock<Value> = OnceLock::new();
        SCHEMA.get_or_init(|| {
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "title": "WeatherDerived",
                "description": "查询指定城市的当前天气。",
                "type": "object",
                "properties": {
                    "city": {
                        "description": "城市名，如 \"beijing\"。",
                        "type": "string"
                    },
                    "unit": {
                        "description": "可选的温度单位，默认摄氏度。",
                        "type": ["string", "null"]
                    }
                },
                "required": ["city"]
            })
        })
    }
    fn execute<'a>(
        &'a self,
        args: Value,
        _ctx: ToolContext,
    ) -> synonz::BoxFuture<'a, Result<ToolResult, ToolError>> {
        Box::pin(async move {
            let typed: WeatherDerived =
                serde_json::from_value(args).map_err(|error| ToolError::InvalidArguments {
                    message: error.to_string(),
                })?;
            typed.run().await
        })
    }
}

// ── Equivalence ──

fn context() -> ToolContext {
    ToolContext::new(synonz::CancellationToken::new())
}

#[tokio::test]
async fn derived_and_manual_names_match() {
    let derived = WeatherDerived {
        city: "beijing".into(),
        unit: None,
    };
    assert_eq!(derived.name(), WeatherManual.name());
}

#[tokio::test]
async fn derived_and_manual_descriptions_match() {
    let derived = WeatherDerived {
        city: "beijing".into(),
        unit: None,
    };
    assert_eq!(derived.description(), WeatherManual.description());
}

#[tokio::test]
async fn derived_schema_matches_expected_shape() {
    let derived = WeatherDerived {
        city: "beijing".into(),
        unit: None,
    };
    let schema = derived.parameters_schema();
    assert_eq!(
        schema,
        &json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "WeatherDerived",
            "description": "查询指定城市的当前天气。",
            "type": "object",
            "properties": {
                "city": {
                    "description": "城市名，如 \"beijing\"。",
                    "type": "string"
                },
                "unit": {
                    "description": "可选的温度单位，默认摄氏度。",
                    "type": ["string", "null"]
                }
            },
            "required": ["city"]
        })
    );
    // And it is stable across calls (cached).
    assert_eq!(schema, derived.parameters_schema());
}

#[tokio::test]
async fn derived_and_manual_execution_match() {
    let args = json!({"city": "beijing"});
    let derived = WeatherDerived {
        city: "beijing".into(),
        unit: None,
    };
    let derived = derived.execute(args.clone(), context()).await.unwrap();
    let manual = WeatherManual.execute(args, context()).await.unwrap();
    assert_eq!(derived, manual);
    assert!(matches!(derived, ToolResult::Ok { .. }));
}

#[tokio::test]
async fn invalid_arguments_map_to_invalid_arguments_error() {
    let derived = WeatherDerived {
        city: "beijing".into(),
        unit: None,
    };
    let error = derived
        .execute(json!({"nope": 1}), context())
        .await
        .expect_err("missing city must fail");
    assert!(matches!(error, ToolError::InvalidArguments { .. }));
}

#[cfg(feature = "test-util")]
#[tokio::test]
async fn derived_tool_drives_a_full_agent_run() {
    use synonz::{Agent, ContentBlock, MockModel, ModelStreamItem, Role, ToolCall};

    let model = MockModel::new(vec![
        vec![ModelStreamItem::Finish {
            message: synonz::Message::new(
                Role::Assistant,
                vec![ContentBlock::ToolCall(ToolCall::new(
                    "x1",
                    "weather_derived",
                    json!({"city": "beijing"}),
                ))],
            ),
            usage: synonz::TokenUsage::new(1, 1),
        }],
        vec![ModelStreamItem::Finish {
            message: synonz::Message::assistant_text("done"),
            usage: synonz::TokenUsage::new(1, 1),
        }],
    ]);
    let agent = Agent::builder()
        .model(model)
        .tool(WeatherDerived {
            city: String::new(), // per-call state comes from the arguments
            unit: None,
        })
        .build()
        .unwrap();

    let output = agent.ask("weather?").await.unwrap();
    assert_eq!(output.text(), Some("done"));
}
