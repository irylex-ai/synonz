//! Regression lock: the schema shape that `#[derive(Tool)]` relies on.
//!
//! The derive generates `parameters_schema` via `schema_for!`, so its
//! output depends on schemars' behavior: doc comments become descriptions,
//! `Option<T>` fields are optional, and the root carries `$schema`/`title`.
//! If a schemars upgrade changes any of this, this test fails first.

use schemars::JsonSchema;
use schemars::schema_for;
use serde_json::json;

/// 查询城市天气。
///
/// 支持全球主要城市。
// Fields are consumed by the JsonSchema derive, which clippy cannot see.
#[allow(dead_code)]
#[derive(JsonSchema)]
struct Weather {
    /// 城市名，如 "beijing"。
    city: String,
    /// 可选的温度单位。
    unit: Option<String>,
}

#[test]
fn schemars_shape_matches_derive_expectations() {
    let schema = serde_json::to_value(schema_for!(Weather)).unwrap();
    assert_eq!(
        schema,
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Weather",
            "description": "查询城市天气。\n\n支持全球主要城市。",
            "type": "object",
            "properties": {
                "city": {
                    "description": "城市名，如 \"beijing\"。",
                    "type": "string"
                },
                "unit": {
                    "description": "可选的温度单位。",
                    "type": ["string", "null"]
                }
            },
            "required": ["city"]
        })
    );
}
