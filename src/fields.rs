//! Declarative prompt fields and bounded prompt configuration.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Text,
    Choice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceOption {
    pub id: String,
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Field {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: FieldKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ChoiceOption>,
}

impl<'de> Deserialize<'de> for Field {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
        enum Definition {
            Text {
                id: String,
                label: String,
                #[serde(default)]
                default: Option<String>,
            },
            Choice {
                id: String,
                label: String,
                #[serde(default)]
                default: Option<String>,
                options: Vec<ChoiceOption>,
            },
        }
        Ok(match Definition::deserialize(deserializer)? {
            Definition::Text { id, label, default } => Self {
                id,
                label,
                default,
                kind: FieldKind::Text,
                options: Vec::new(),
            },
            Definition::Choice {
                id,
                label,
                default,
                options,
            } => Self {
                id,
                label,
                default,
                kind: FieldKind::Choice,
                options,
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PromptSettings {
    pub ack_timeout_ms: u64,
    pub lease_renewal_ms: u64,
    pub lease_expiry_ms: u64,
    pub completion_timeout_ms: u64,
    pub release_timeout_ms: u64,
    pub close_timeout_ms: u64,
    pub focus_settle_ms: u64,
    pub max_connections: usize,
    pub max_frame_bytes: usize,
    pub max_queued_messages: usize,
    pub max_fields: usize,
    pub max_options: usize,
    pub max_answer_bytes: usize,
    pub max_request_bytes: usize,
    pub max_output_bytes: usize,
}
impl Default for PromptSettings {
    fn default() -> Self {
        Self {
            ack_timeout_ms: 2000,
            lease_renewal_ms: 5000,
            lease_expiry_ms: 15000,
            completion_timeout_ms: 300000,
            release_timeout_ms: 2000,
            close_timeout_ms: 2000,
            focus_settle_ms: 150,
            max_connections: 16,
            max_frame_bytes: 65536,
            max_queued_messages: 32,
            max_fields: 64,
            max_options: 128,
            max_answer_bytes: 4096,
            max_request_bytes: 65536,
            max_output_bytes: 4000,
        }
    }
}
impl PromptSettings {
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("ack_timeout_ms", self.ack_timeout_ms),
            ("lease_renewal_ms", self.lease_renewal_ms),
            ("lease_expiry_ms", self.lease_expiry_ms),
            ("completion_timeout_ms", self.completion_timeout_ms),
            ("release_timeout_ms", self.release_timeout_ms),
            ("close_timeout_ms", self.close_timeout_ms),
        ] {
            if value == 0 || value > 86400000 {
                bail!("prompt.{name} must be between 1 and 86400000");
            }
        }
        if self.lease_renewal_ms >= self.lease_expiry_ms {
            bail!("prompt.lease_renewal_ms must be less than lease_expiry_ms");
        }
        if self.focus_settle_ms > 60000 {
            bail!("prompt.focus_settle_ms must be at most 60000");
        }
        for (name, value, maximum) in [
            ("max_connections", self.max_connections, 16),
            ("max_frame_bytes", self.max_frame_bytes, 65536),
            ("max_queued_messages", self.max_queued_messages, 32),
            ("max_fields", self.max_fields, 64),
            ("max_options", self.max_options, 128),
            ("max_answer_bytes", self.max_answer_bytes, 4096),
            ("max_request_bytes", self.max_request_bytes, 65536),
            ("max_output_bytes", self.max_output_bytes, 4000),
        ] {
            if value == 0 || value > maximum {
                bail!("prompt.{name} must be between 1 and {maximum}");
            }
        }
        Ok(())
    }
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_alphanumeric() || c == '_')
}
pub fn validate(fields: &[Field], settings: &PromptSettings) -> Result<()> {
    if fields.len() > settings.max_fields {
        bail!("too many prompt fields");
    }
    let mut ids = HashSet::new();
    for field in fields {
        if !valid_id(&field.id) || !ids.insert(&field.id) {
            bail!("field IDs must be unique letters, numbers, or underscores");
        }
        if field.label.trim().is_empty() {
            bail!("field '{}' requires a label", field.id);
        }
        if field.kind == FieldKind::Text {
            if !field.options.is_empty() {
                bail!("text field '{}' cannot have options", field.id);
            }
            if field
                .default
                .as_ref()
                .is_some_and(|v| v.len() > settings.max_answer_bytes)
            {
                bail!("text default exceeds answer limit");
            }
        } else {
            if field.options.is_empty() || field.options.len() > settings.max_options {
                bail!(
                    "choice field '{}' requires a bounded nonempty options list",
                    field.id
                );
            }
            let mut options = HashSet::new();
            for option in &field.options {
                if !valid_id(&option.id)
                    || !options.insert(&option.id)
                    || option.label.trim().is_empty()
                {
                    bail!("choice options require unique IDs and nonempty labels");
                }
                if option.id.len() > settings.max_answer_bytes
                    || option.value.len() > settings.max_answer_bytes
                {
                    bail!("choice option exceeds answer limit");
                }
            }
            if field
                .default
                .as_ref()
                .is_some_and(|id| !options.contains(id))
            {
                bail!("choice default must identify a configured option");
            }
        }
    }
    // Reserve a conservative bounded envelope for instance, session and request IDs.
    let size = serde_json::to_vec(fields)?.len().saturating_add(1024);
    if !fields.is_empty() && size > settings.max_request_bytes.min(settings.max_frame_bytes) {
        bail!("prompt definitions and request envelope exceed encoded request limit");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn field(doc: serde_json::Value) -> Result<Field> {
        Ok(serde_json::from_value(doc)?)
    }
    #[test]
    fn field_shape_is_strict_and_defaults_identify_choices() {
        use serde_json::json;
        assert!(field(json!({"id":"a","label":"A","type":"text","options":[]})).is_err());
        assert!(field(json!({"id":"a","label":"A","type":"choice"})).is_err());
        let f = field(json!({"id":"a","label":"A","type":"choice","default":"bad","options":[{"id":"yes","label":"Yes","value":"literal"}]})).unwrap();
        assert!(validate(&[f], &PromptSettings::default()).is_err());
    }
    #[test]
    fn resource_limits_are_positive_and_native_limit_cannot_grow() {
        let mut settings = PromptSettings {
            max_output_bytes: 4001,
            ..Default::default()
        };
        assert!(settings.validate().is_err());
        settings.max_output_bytes = 4000;
        settings.ack_timeout_ms = 0;
        assert!(settings.validate().is_err());
        assert!(PromptSettings::default().validate().is_ok());
    }
    #[test]
    fn schema_and_runtime_agree_on_field_structure() {
        use serde_json::json;
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../schemas/match.schema.json")).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        for (definition, valid) in [
            (json!({"id":"value","label":"Value","type":"text"}), true),
            (
                json!({"id":"value","label":"Value","type":"text","options":[]}),
                false,
            ),
            (json!({"id":"value","label":"Value","type":"choice"}), false),
            (
                json!({"id":"value","label":"Value","type":"text","other":true}),
                false,
            ),
        ] {
            assert_eq!(validator.is_valid(&json!({"matches":[{"trigger":";form","replace":"{{value}}","fields":[definition.clone()]}]})), valid);
            assert_eq!(field(definition).is_ok(), valid);
        }
    }
}
