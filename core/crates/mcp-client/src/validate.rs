//! Checking a proposed tool call against the tool's own schema.
//!
//! # Why this happens before a human sees it
//!
//! MCP `tools/list` returns a JSON Schema per tool. A model-authored argument object shown to a
//! user unvalidated invites the worst version of this feature: a plausible confirmation dialog for
//! a call that will fail, or worse, one whose extra field means something to the server that the
//! user was never asked about.
//!
//! A validation failure goes back to the model as an observation, the same recovery path the agent
//! already uses for an unparseable action — the model gets to correct itself rather than the run
//! dying.
//!
//! # Why unknown fields are rejected rather than dropped
//!
//! Dropping them silently would mean the call the user confirmed is not the call the model
//! proposed, which defeats the confirmation. Rejecting them means the model tries again.
//!
//! # What this is not
//!
//! Not a complete JSON Schema implementation. It covers the subset MCP servers actually publish for
//! tool arguments — an object with typed properties, a required list, and enums — and says so when
//! it meets something it does not understand rather than passing it as valid. Claiming to validate
//! and not doing so is worse than declining.

use serde_json::Value;

/// Why a proposed argument object was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    /// The arguments were not a JSON object.
    NotAnObject,
    /// A field the schema requires is absent.
    MissingRequired { field: String },
    /// A field is present with the wrong type.
    WrongType {
        field: String,
        expected: String,
        got: String,
    },
    /// A field the schema does not mention.
    ///
    /// Refused rather than dropped: silently removing it would mean the call the user confirmed is
    /// not the call that was proposed.
    UnknownField { field: String },
    /// A value outside the schema's enumeration.
    NotInEnum { field: String, allowed: Vec<String> },
    /// The schema itself uses something this validator does not implement.
    ///
    /// Reported rather than ignored. Treating an unsupported schema as satisfied would be claiming
    /// a check that never happened.
    UnsupportedSchema { reason: String },
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Invalid::NotAnObject => write!(f, "the arguments must be a JSON object"),
            Invalid::MissingRequired { field } => write!(f, "'{field}' is required"),
            Invalid::WrongType {
                field,
                expected,
                got,
            } => write!(f, "'{field}' must be {expected}, got {got}"),
            Invalid::UnknownField { field } => {
                write!(f, "'{field}' is not a parameter of this tool")
            }
            Invalid::NotInEnum { field, allowed } => {
                write!(f, "'{field}' must be one of: {}", allowed.join(", "))
            }
            Invalid::UnsupportedSchema { reason } => {
                write!(f, "this tool's schema is not one I can check: {reason}")
            }
        }
    }
}

/// Validate `arguments` against a tool's `input_schema`.
///
/// Returns every problem found rather than the first, so a model correcting itself gets the whole
/// picture in one observation instead of discovering them one round trip at a time.
pub fn validate(schema: &Value, arguments: &Value) -> Vec<Invalid> {
    let Some(args) = arguments.as_object() else {
        return vec![Invalid::NotAnObject];
    };

    // A tool with no schema, or an empty one, accepts anything — that is the server's choice to
    // publish and not this validator's to second-guess.
    let Some(schema_obj) = schema.as_object() else {
        return Vec::new();
    };
    if schema_obj.is_empty() {
        return Vec::new();
    }

    if let Some(kind) = schema_obj.get("type").and_then(Value::as_str) {
        if kind != "object" {
            return vec![Invalid::UnsupportedSchema {
                reason: format!("the top level is '{kind}' rather than an object"),
            }];
        }
    }

    // Composition keywords change what "valid" means and are not implemented. Passing them as
    // satisfied would be claiming a check that never happened.
    for keyword in ["oneOf", "anyOf", "allOf", "not", "$ref"] {
        if schema_obj.contains_key(keyword) {
            return vec![Invalid::UnsupportedSchema {
                reason: format!("it uses '{keyword}'"),
            }];
        }
    }

    let properties = schema_obj
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let required: Vec<String> = schema_obj
        .get("required")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let mut problems = Vec::new();

    for field in &required {
        // A required field explicitly set to null is absent for this purpose: a server asking for a
        // ticket title does not want the string "null".
        if !args.contains_key(field) || args[field].is_null() {
            problems.push(Invalid::MissingRequired {
                field: field.clone(),
            });
        }
    }

    // `additionalProperties: true` is a server saying it will take extras, so they are allowed.
    let extras_allowed = schema_obj
        .get("additionalProperties")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    for (field, value) in args {
        let Some(spec) = properties.get(field) else {
            if !extras_allowed && !properties.is_empty() {
                problems.push(Invalid::UnknownField {
                    field: field.clone(),
                });
            }
            continue;
        };

        // An explicit null for an optional field is the model saying "not provided", which is not a
        // type error worth sending back.
        if value.is_null() {
            continue;
        }

        if let Some(expected) = spec.get("type").and_then(Value::as_str) {
            if let Some(problem) = check_type(field, expected, value) {
                problems.push(problem);
                // No point checking the enum of a value that is the wrong shape.
                continue;
            }
        }

        if let Some(allowed) = spec.get("enum").and_then(Value::as_array) {
            let permitted: Vec<String> = allowed.iter().map(render).collect();
            if !allowed.iter().any(|a| a == value) {
                problems.push(Invalid::NotInEnum {
                    field: field.clone(),
                    allowed: permitted,
                });
            }
        }
    }

    problems
}

fn check_type(field: &str, expected: &str, value: &Value) -> Option<Invalid> {
    let matches = match expected {
        "string" => value.is_string(),
        "number" => value.is_number(),
        // An integer schema accepts 3 but not 3.5. A model that writes 3.0 for a count is common
        // enough to allow, since it is the same number.
        "integer" => value.as_i64().is_some() || value.as_f64().is_some_and(|n| n.fract() == 0.0),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        // An unrecognised type name is not something to guess at.
        _ => return None,
    };

    (!matches).then(|| Invalid::WrongType {
        field: field.to_string(),
        expected: expected.to_string(),
        got: type_name(value).to_string(),
    })
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn render(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ticket_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "priority": { "type": "string", "enum": ["low", "high"] },
                "estimate": { "type": "integer" },
                "labels": { "type": "array" },
                "notify": { "type": "boolean" }
            },
            "required": ["title"]
        })
    }

    #[test]
    fn a_well_formed_call_passes() {
        let problems = validate(
            &ticket_schema(),
            &json!({ "title": "Fix the importer", "priority": "high", "estimate": 3 }),
        );
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn a_missing_required_field_is_reported() {
        let problems = validate(&ticket_schema(), &json!({ "priority": "high" }));
        assert_eq!(
            problems,
            vec![Invalid::MissingRequired {
                field: "title".into()
            }]
        );
    }

    /// A server asking for a title does not want the string "null".
    #[test]
    fn a_required_field_set_to_null_counts_as_missing() {
        let problems = validate(&ticket_schema(), &json!({ "title": null }));
        assert_eq!(
            problems,
            vec![Invalid::MissingRequired {
                field: "title".into()
            }]
        );
    }

    /// Dropping it would mean the call the user confirmed is not the call proposed.
    #[test]
    fn an_unknown_field_is_refused_rather_than_dropped() {
        let problems = validate(
            &ticket_schema(),
            &json!({ "title": "x", "assignee_id": "u-1" }),
        );
        assert_eq!(
            problems,
            vec![Invalid::UnknownField {
                field: "assignee_id".into()
            }]
        );
    }

    #[test]
    fn extras_are_allowed_when_the_server_says_so() {
        let mut schema = ticket_schema();
        schema["additionalProperties"] = json!(true);

        let problems = validate(&schema, &json!({ "title": "x", "whatever": 1 }));
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn wrong_types_are_reported_with_what_was_expected() {
        let problems = validate(
            &ticket_schema(),
            &json!({ "title": 42, "notify": "yes", "labels": "bug" }),
        );

        assert!(problems.contains(&Invalid::WrongType {
            field: "title".into(),
            expected: "string".into(),
            got: "number".into()
        }));
        assert!(problems.contains(&Invalid::WrongType {
            field: "notify".into(),
            expected: "boolean".into(),
            got: "string".into()
        }));
        assert!(problems.contains(&Invalid::WrongType {
            field: "labels".into(),
            expected: "array".into(),
            got: "string".into()
        }));
    }

    /// A model writing 3.0 for a count means three.
    #[test]
    fn an_integer_accepts_a_whole_float_but_not_a_fraction() {
        assert!(validate(&ticket_schema(), &json!({ "title": "x", "estimate": 3.0 })).is_empty());

        let problems = validate(&ticket_schema(), &json!({ "title": "x", "estimate": 3.5 }));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(matches!(problems[0], Invalid::WrongType { .. }));
    }

    #[test]
    fn a_value_outside_the_enum_is_reported_with_the_options() {
        let problems = validate(
            &ticket_schema(),
            &json!({ "title": "x", "priority": "urgent" }),
        );
        assert_eq!(
            problems,
            vec![Invalid::NotInEnum {
                field: "priority".into(),
                allowed: vec!["low".into(), "high".into()]
            }]
        );
    }

    #[test]
    fn an_optional_field_set_to_null_is_not_provided_rather_than_wrong() {
        let problems = validate(&ticket_schema(), &json!({ "title": "x", "priority": null }));
        assert!(problems.is_empty(), "{problems:?}");
    }

    /// A model correcting itself should get the whole picture in one observation.
    #[test]
    fn every_problem_is_reported_not_just_the_first() {
        let problems = validate(&ticket_schema(), &json!({ "notify": "yes", "extra": 1 }));
        assert_eq!(problems.len(), 3, "{problems:?}");
    }

    #[test]
    fn arguments_that_are_not_an_object_are_refused() {
        assert_eq!(
            validate(&ticket_schema(), &json!("just a string")),
            vec![Invalid::NotAnObject]
        );
        assert_eq!(
            validate(&ticket_schema(), &json!([1, 2])),
            vec![Invalid::NotAnObject]
        );
    }

    /// Claiming to validate and not doing so is worse than declining.
    #[test]
    fn a_schema_this_validator_cannot_check_is_reported_not_waved_through() {
        for keyword in ["oneOf", "anyOf", "allOf", "not", "$ref"] {
            let schema = json!({ "type": "object", keyword: [] });
            let problems = validate(&schema, &json!({ "anything": 1 }));
            assert!(
                matches!(problems.first(), Some(Invalid::UnsupportedSchema { .. })),
                "{keyword} should be reported: {problems:?}"
            );
        }
    }

    #[test]
    fn a_non_object_top_level_schema_is_reported() {
        let problems = validate(&json!({ "type": "array" }), &json!({}));
        assert!(matches!(
            problems.first(),
            Some(Invalid::UnsupportedSchema { .. })
        ));
    }

    /// A tool that publishes no schema accepts anything, which is the server's choice to make.
    #[test]
    fn an_absent_or_empty_schema_accepts_anything() {
        assert!(validate(&json!({}), &json!({ "whatever": 1 })).is_empty());
        assert!(validate(&json!(null), &json!({ "whatever": 1 })).is_empty());
    }

    #[test]
    fn a_schema_with_no_properties_does_not_reject_everything() {
        // `required` with no `properties` is unusual but legal, and rejecting every field would make
        // such a tool uncallable.
        let schema = json!({ "type": "object", "required": ["title"] });
        let problems = validate(&schema, &json!({ "title": "x", "extra": 1 }));
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn the_messages_name_the_field_and_the_fix() {
        let rendered = Invalid::MissingRequired {
            field: "title".into(),
        }
        .to_string();
        assert!(rendered.contains("title"), "{rendered}");

        let rendered = Invalid::NotInEnum {
            field: "priority".into(),
            allowed: vec!["low".into(), "high".into()],
        }
        .to_string();
        assert!(rendered.contains("low, high"), "{rendered}");
    }
}
