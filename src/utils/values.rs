//! Conversions between JSON values and the Hrana wire format.
//!
//! Hrana (the protocol Turso/sqld speak over HTTP) encodes every value as a
//! tagged object, e.g. `{"type":"integer","value":"42"}`. Integers are sent as
//! strings to survive 64-bit precision, blobs as base64. These helpers are pure
//! and fully unit-tested; the rusqlite (local) conversions live in `client.rs`
//! because they depend on the SQLite value type.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};

/// Convert a bound parameter (JSON) into a Hrana argument object.
pub fn json_to_hrana_arg(value: &Value) -> Value {
    match value {
        Value::Null => json!({ "type": "null" }),
        // Hrana has no boolean type; SQLite stores booleans as 0/1.
        Value::Bool(b) => json!({ "type": "integer", "value": if *b { "1" } else { "0" } }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                json!({ "type": "integer", "value": i.to_string() })
            } else if let Some(f) = n.as_f64() {
                json!({ "type": "float", "value": f })
            } else {
                json!({ "type": "text", "value": n.to_string() })
            }
        }
        Value::String(s) => json!({ "type": "text", "value": s }),
        // Arrays/objects are stored as their JSON text representation.
        other => json!({ "type": "text", "value": other.to_string() }),
    }
}

/// Convert a Hrana value object from a result row into plain JSON.
pub fn hrana_value_to_json(value: &Value) -> Value {
    match value.get("type").and_then(Value::as_str) {
        Some("null") | None => Value::Null,
        Some("integer") => match value.get("value") {
            // Integers arrive as strings; keep precision, fall back to text.
            Some(Value::String(s)) => s
                .parse::<i64>()
                .map(|i| json!(i))
                .unwrap_or_else(|_| Value::String(s.clone())),
            Some(Value::Number(n)) => Value::Number(n.clone()),
            _ => Value::Null,
        },
        Some("float") => match value.get("value") {
            Some(Value::Number(n)) => Value::Number(n.clone()),
            Some(Value::String(s)) => s
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            _ => Value::Null,
        },
        Some("text") => value.get("value").cloned().unwrap_or(Value::Null),
        // Re-encode blobs as standard base64 so the grid gets a stable string.
        Some("blob") => match value.get("base64").and_then(Value::as_str) {
            Some(b64) => match STANDARD.decode(b64) {
                Ok(bytes) => Value::String(STANDARD.encode(bytes)),
                Err(_) => Value::String(b64.to_string()),
            },
            None => Value::Null,
        },
        Some(_) => value.get("value").cloned().unwrap_or(Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_args() {
        assert_eq!(json_to_hrana_arg(&Value::Null), json!({"type":"null"}));
        assert_eq!(
            json_to_hrana_arg(&json!(true)),
            json!({"type":"integer","value":"1"})
        );
        assert_eq!(
            json_to_hrana_arg(&json!(42)),
            json!({"type":"integer","value":"42"})
        );
        assert_eq!(
            json_to_hrana_arg(&json!("hi")),
            json!({"type":"text","value":"hi"})
        );
        assert_eq!(
            json_to_hrana_arg(&json!({"k": 1})),
            json!({"type":"text","value":"{\"k\":1}"})
        );
    }

    #[test]
    fn float_args_stay_numeric() {
        assert_eq!(
            json_to_hrana_arg(&json!(1.5)),
            json!({"type":"float","value":1.5})
        );
    }

    #[test]
    fn decodes_integer_from_string() {
        assert_eq!(
            hrana_value_to_json(&json!({"type":"integer","value":"123"})),
            json!(123)
        );
    }

    #[test]
    fn decodes_null_and_text() {
        assert_eq!(hrana_value_to_json(&json!({"type":"null"})), Value::Null);
        assert_eq!(
            hrana_value_to_json(&json!({"type":"text","value":"abc"})),
            json!("abc")
        );
    }

    #[test]
    fn decodes_float() {
        assert_eq!(
            hrana_value_to_json(&json!({"type":"float","value":2.25})),
            json!(2.25)
        );
    }

    #[test]
    fn roundtrips_blob_base64() {
        let b64 = STANDARD.encode([1u8, 2, 3]);
        assert_eq!(
            hrana_value_to_json(&json!({"type":"blob","base64": b64.clone()})),
            json!(b64)
        );
    }
}
