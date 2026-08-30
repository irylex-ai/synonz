//! Derive macros for Synonz, including `#[derive(Tool)]`.
//!
//! # `#[derive(Tool)]`
//!
//! Turns a struct into a `Tool` implementation whose fields are the tool's
//! arguments. The struct must also derive `Deserialize` and `JsonSchema`
//! (both re-exported by `synonz`) and define
//! `async fn run(&self) -> Result<ToolResult, ToolError>`:
//!
//! ```ignore
//! // Cannot compile here: `synonz` depends on this crate, not the other
//! // way around. The compile-verified example lives in `synonz`'s crate
//! // documentation and in `synonz/tests/derive.rs`.
//! use synonz::{Deserialize, JsonSchema, Tool, ToolContent, ToolResult};
//!
//! /// Queries the current weather for a city.
//! #[derive(Tool, Deserialize, JsonSchema)]
//! struct Weather {
//!     /// The city name.
//!     city: String,
//! }
//!
//! impl Weather {
//!     async fn run(&self) -> Result<ToolResult, ToolError> {
//!         Ok(ToolResult::Ok {
//!             content: ToolContent::Text {
//!                 text: format!("{}: sunny", self.city),
//!             },
//!         })
//!     }
//! }
//! ```
//!
//! ## Generated parts
//!
//! - `name`: the struct name in `snake_case`;
//! - `description`: the struct's doc comment (trimmed);
//! - `parameters_schema`: the JSON Schema of the struct, generated via
//!   `schema_for!` and cached — doc comments on fields
//!   become property descriptions;
//! - `execute`: deserializes the call arguments into a fresh `Self` and
//!   awaits `Self::run(&self)`.
//!
//! ## Requirements
//!
//! - the struct must also derive `Deserialize` and `JsonSchema` (both
//!   re-exported by `synonz`);
//! - the struct must define an associated `async fn run(&self) ->
//!   Result<ToolResult, ToolError>` (same crate visibility rules apply).
//!
//! ## Semantics
//!
//! Per-call state comes from the deserialized arguments; the registered
//! instance's own field values are *not* carried into calls (the type
//! doubles as its own argument schema). Tools that need genuine internal
//! state should implement `Tool` directly.

use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Meta};

/// Derives the `Tool` implementation. See the crate
/// documentation for usage and requirements.
#[proc_macro_derive(Tool)]
pub fn derive_tool(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand(&input)
        .unwrap_or_else(|error| error.to_compile_error())
        .into()
}

fn expand(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let ident = &input.ident;

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    input,
                    "#[derive(Tool)] requires a struct with named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "#[derive(Tool)] can only be applied to structs",
            ));
        }
    };
    let _ = fields; // schema generation goes through schemars, not field enumeration

    let tool_name = to_snake_case(&ident.to_string());
    let description = doc_comment(&input.attrs);

    Ok(quote! {
        impl ::synonz::Tool for #ident {
            fn name(&self) -> &str {
                #tool_name
            }

            fn description(&self) -> &str {
                #description
            }

            fn parameters_schema(&self) -> &::synonz::serde_json::Value {
                static SCHEMA: ::std::sync::OnceLock<::synonz::serde_json::Value> =
                    ::std::sync::OnceLock::new();
                SCHEMA.get_or_init(|| {
                    // Serializing a schemars-generated schema cannot fail.
                    ::synonz::serde_json::to_value(::synonz::schema_for!(#ident))
                        .expect("tool schema serialization")
                })
            }

            fn execute<'a>(
                &'a self,
                args: ::synonz::serde_json::Value,
                _ctx: ::synonz::ToolContext,
            ) -> ::synonz::BoxFuture<'a, ::std::result::Result<::synonz::ToolResult, ::synonz::ToolError>>
            {
                ::std::boxed::Box::pin(async move {
                    let this: Self = match ::synonz::serde_json::from_value(args) {
                        ::std::result::Result::Ok(typed) => typed,
                        ::std::result::Result::Err(error) => {
                            return ::std::result::Result::Err(::synonz::ToolError::InvalidArguments {
                                message: error.to_string(),
                            });
                        }
                    };
                    this.run().await
                })
            }
        }
    })
}

/// Extracts the doc comment text from attributes (joined, trimmed).
fn doc_comment(attrs: &[syn::Attribute]) -> String {
    let mut doc = String::new();
    for attribute in attrs {
        if !attribute.path().is_ident("doc") {
            continue;
        }
        if let Meta::NameValue(name_value) = &attribute.meta
            && let syn::Expr::Lit(lit) = &name_value.value
            && let syn::Lit::Str(text) = &lit.lit
        {
            doc.push_str(text.value().trim());
            doc.push('\n');
        }
    }
    doc.trim().to_string()
}

/// Converts a Rust identifier to `snake_case`.
fn to_snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (index, &current) in chars.iter().enumerate() {
        if current.is_uppercase() {
            let previous_lower = index > 0 && chars[index - 1].is_lowercase();
            let next_lower = index + 1 < chars.len() && chars[index + 1].is_lowercase();
            if index > 0 && (previous_lower || next_lower) {
                out.push('_');
            }
            out.extend(current.to_lowercase());
        } else {
            out.push(current);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::to_snake_case;

    #[test]
    fn snake_case_conversion() {
        assert_eq!(to_snake_case("Weather"), "weather");
        assert_eq!(to_snake_case("WeatherApi"), "weather_api");
        assert_eq!(to_snake_case("WeatherAPI"), "weather_api");
        assert_eq!(to_snake_case("HTTPStatus"), "http_status");
        assert_eq!(to_snake_case("already_snake"), "already_snake");
    }
}
