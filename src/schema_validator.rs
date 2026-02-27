use jsonschema::JSONSchema;
use once_cell::sync::Lazy;
use serde_json::{json, Value};

static LOG_SCHEMA: Lazy<JSONSchema> = Lazy::new(|| {
    let schema = json!({
        "type": "object",
        "required": ["level", "source", "message"],
        "properties": {
            "level": {
                "type": "string",
                "enum": ["debug", "info", "warn", "error"]
            },
            "source": {
                "type": "string",
                "minLength": 1,
                "maxLength": 128
            },
            "message": {
                "type": "string",
                "minLength": 1,
                "maxLength": 8192
            },
            "metadata": {
                "type": ["object", "null"]
            }
        },
        "additionalProperties": true
    });
    JSONSchema::compile(&schema).expect("Invalid schema")
});

pub struct SchemaValidator;

impl SchemaValidator {
    pub fn validate(value: &Value) -> Result<(), String> {
        let result = LOG_SCHEMA.validate(value);
        if let Err(errors) = result {
            let msgs: Vec<String> = errors.map(|e| e.to_string()).collect();
            return Err(msgs.join("; "));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_valid_log_entry_passes_schema() {
        let v = json!({ "level": "info", "source": "my-service", "message": "everything ok" });
        assert!(SchemaValidator::validate(&v).is_ok());
    }

    #[test]
    fn test_missing_required_field_fails() {
        let v = json!({ "level": "info", "source": "svc" });
        assert!(SchemaValidator::validate(&v).is_err());
    }

    #[test]
    fn test_invalid_level_fails() {
        let v = json!({ "level": "CRITICAL", "source": "svc", "message": "oops" });
        assert!(SchemaValidator::validate(&v).is_err());
    }

    #[test]
    fn test_empty_message_fails() {
        let v = json!({ "level": "info", "source": "svc", "message": "" });
        assert!(SchemaValidator::validate(&v).is_err());
    }
}
