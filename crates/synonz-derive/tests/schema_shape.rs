//! Regression lock: the schema shape that `#[derive(Tool)]` relies on.
//!
//! The derive generates `parameters_schema` via `schema_for!`, so its
//! output depends on schemars' behavior: doc comments become descriptions,
//! `Option<T>` fields are optional, and the root carries `$schema`/`title`.
//! If a schemars upgrade changes any of this, this test fails first.

use schemars::JsonSchema;
use schemars::schema_for;
use serde_json::json;

/// Queries the current weather for a city.
///
/// Supports major cities worldwide.
// Fields are consumed by the JsonSchema derive, which clippy cannot see.
#[allow(dead_code)]
#[derive(JsonSchema)]
struct Weather {
    /// The city name, e.g. "beijing".
    city: String,
    /// Optional temperature unit.
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
            "description": "Queries the current weather for a city.\n\nSupports major cities worldwide.",
            "type": "object",
            "properties": {
                "city": {
                    "description": "The city name, e.g. \"beijing\".",
                    "type": "string"
                },
                "unit": {
                    "description": "Optional temperature unit.",
                    "type": ["string", "null"]
                }
            },
            "required": ["city"]
        })
    );
}
