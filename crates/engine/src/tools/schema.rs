//! The JSON Schema subset tool inputs are written in, and a validator for it.
//!
//! The subset is what strict tool use accepts, plus a few constraints the engine checks itself:
//!
//! * every object has `"additionalProperties": false` and lists **every** property in `required`
//!   (an optional value is nullable instead: `"type": ["integer", "null"]`);
//! * keywords: `type`, `description`, `properties`, `required`, `additionalProperties`, `items`,
//!   `enum`, and the engine-checked `minimum`, `maximum`, `minItems`, `maxItems`, `maxLength`;
//! * no `$ref`, `anyOf`, formats or patterns.
//!
//! Strict tool use rejects the numeric, length and item-count constraints, so
//! [`for_strict_api`] moves them into the description (as the official SDKs do) and the engine
//! validates them in [`validate`].

use serde_json::{Map, Value};

/// Keywords a schema may use.
const KEYWORDS: &[&str] =
    &["type", "description", "properties", "required", "additionalProperties", "items", "enum", "minimum", "maximum", "minItems", "maxItems", "maxLength"];
/// Keywords strict tool use does not accept (validated by the engine instead).
const ENGINE_ONLY: &[&str] = &["minimum", "maximum", "minItems", "maxItems", "maxLength"];
const TYPES: &[&str] = &["object", "array", "string", "integer", "number", "boolean", "null"];
/// Nesting depth of schemas and of validated values.
const MAX_DEPTH: usize = 16;

/// The types a schema allows.
fn types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

/// Whether `schema` is in the subset (a tool's input schema: an object at the top).
pub fn check_strict(schema: &Value) -> Result<(), String> {
    if types(schema) != ["object"] {
        return Err("the input schema must have \"type\": \"object\"".into());
    }
    check_node(schema, "$", 0)
}

fn check_node(schema: &Value, path: &str, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("{path}: nested too deeply"));
    }
    let obj = schema.as_object().ok_or_else(|| format!("{path}: a schema must be an object"))?;
    if let Some(k) = obj.keys().find(|k| !KEYWORDS.contains(&k.as_str())) {
        return Err(format!("{path}: keyword \"{k}\" is outside the strict subset"));
    }
    let ts = types(schema);
    let declared = match obj.get("type") {
        Some(Value::String(_)) => 1,
        Some(Value::Array(a)) => a.len(),
        _ => 0,
    };
    if ts.is_empty() || ts.len() != declared || ts.iter().any(|t| !TYPES.contains(t)) {
        return Err(format!("{path}: \"type\" must be one of {TYPES:?} or a list of them"));
    }
    if ts.iter().enumerate().any(|(i, t)| ts.iter().skip(i + 1).any(|u| u == t)) {
        return Err(format!("{path}: \"type\" lists a type twice"));
    }
    if obj.get("description").is_some_and(|d| !d.is_string()) {
        return Err(format!("{path}: \"description\" must be a string"));
    }
    if let Some(e) = obj.get("enum") {
        let a = e.as_array().filter(|a| !a.is_empty()).ok_or_else(|| format!("{path}: \"enum\" must be a non-empty list"))?;
        if let Some(bad) = a.iter().find(|v| !v.is_null() && !matches_type(&ts, v)) {
            return Err(format!("{path}: enum value {bad} does not have the declared type"));
        }
    }
    for k in ["minimum", "maximum"] {
        if obj.get(k).is_some_and(|v| !v.is_number()) || (obj.contains_key(k) && !ts.iter().any(|t| matches!(*t, "integer" | "number"))) {
            return Err(format!("{path}: \"{k}\" needs a number on a numeric type"));
        }
    }
    for (k, ty) in [("minItems", "array"), ("maxItems", "array"), ("maxLength", "string")] {
        if obj.get(k).is_some_and(|v| v.as_u64().is_none()) || (obj.contains_key(k) && !ts.contains(&ty)) {
            return Err(format!("{path}: \"{k}\" needs a whole number on a{} {ty}", if ty == "array" { "n" } else { "" }));
        }
    }
    let is_object = ts.contains(&"object");
    for k in ["properties", "required", "additionalProperties"] {
        if obj.contains_key(k) != is_object {
            return Err(format!("{path}: \"{k}\" belongs on (and is required for) object schemas"));
        }
    }
    if is_object {
        if obj.get("additionalProperties") != Some(&Value::Bool(false)) {
            return Err(format!("{path}: objects need \"additionalProperties\": false"));
        }
        let props = obj.get("properties").and_then(Value::as_object).ok_or_else(|| format!("{path}: \"properties\" must be an object"))?;
        let req = obj.get("required").and_then(Value::as_array).ok_or_else(|| format!("{path}: \"required\" must be a list"))?;
        let req: Vec<&str> = req.iter().filter_map(Value::as_str).collect();
        if req.len() != props.len() || props.keys().any(|k| !req.contains(&k.as_str())) {
            return Err(format!("{path}: \"required\" must list every property (make optional ones nullable)"));
        }
        for (k, p) in props {
            check_node(p, &format!("{path}.{k}"), depth + 1)?;
        }
    }
    if obj.contains_key("items") != ts.contains(&"array") {
        return Err(format!("{path}: arrays need \"items\" (and only arrays have it)"));
    }
    if let Some(items) = obj.get("items") {
        check_node(items, &format!("{path}[]"), depth + 1)?;
    }
    Ok(())
}

fn matches_type(ts: &[&str], v: &Value) -> bool {
    ts.iter().any(|t| match *t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e15),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        _ => false,
    })
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Check `value` against `schema` (a schema in the subset). The error names the offending path.
pub fn validate(schema: &Value, value: &Value) -> Result<(), String> {
    validate_at(schema, value, "input", 0)
}

fn validate_at(schema: &Value, v: &Value, path: &str, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("{path}: nested too deeply"));
    }
    let ts = types(schema);
    if !matches_type(&ts, v) {
        return Err(format!("{path}: expected {}, got {}", ts.join(" or "), type_name(v)));
    }
    if v.is_null() {
        return Ok(());
    }
    if let Some(e) = schema.get("enum").and_then(Value::as_array)
        && !e.contains(v)
    {
        let opts: Vec<String> = e.iter().map(Value::to_string).collect();
        return Err(format!("{path}: {v} is not one of {}", opts.join(", ")));
    }
    if let Some(f) = v.as_f64() {
        if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
            && f < min
        {
            return Err(format!("{path}: {f} is below the minimum {min}"));
        }
        if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
            && f > max
        {
            return Err(format!("{path}: {f} is above the maximum {max}"));
        }
    }
    if let (Some(s), Some(max)) = (v.as_str(), schema.get("maxLength").and_then(Value::as_u64))
        && s.chars().count() as u64 > max
    {
        return Err(format!("{path}: longer than {max} characters"));
    }
    if let Some(a) = v.as_array() {
        // counts first: a huge list is refused before any element is looked at
        let n = a.len() as u64;
        if let Some(max) = schema.get("maxItems").and_then(Value::as_u64)
            && n > max
        {
            return Err(format!("{path}: {n} items, at most {max} allowed"));
        }
        if let Some(min) = schema.get("minItems").and_then(Value::as_u64)
            && n < min
        {
            return Err(format!("{path}: {n} items, at least {min} needed"));
        }
        if let Some(items) = schema.get("items") {
            for (i, x) in a.iter().enumerate() {
                validate_at(items, x, &format!("{path}[{i}]"), depth + 1)?;
            }
        }
    }
    if let Some(o) = v.as_object() {
        let empty = Map::new();
        let props = schema.get("properties").and_then(Value::as_object).unwrap_or(&empty);
        if let Some(k) = o.keys().find(|k| !props.contains_key(*k)) {
            let accepted: Vec<&str> = props.keys().map(String::as_str).collect();
            let accepted = if accepted.is_empty() { "no fields".to_string() } else { accepted.join(", ") };
            return Err(format!("{path}: unknown field \"{k}\"; expected: {accepted}"));
        }
        for k in schema.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            if !o.contains_key(k) {
                return Err(format!("{path}: missing field \"{k}\" (pass null to leave it out)"));
            }
        }
        for (k, x) in o {
            if let Some(p) = props.get(k) {
                validate_at(p, x, &format!("{path}.{k}"), depth + 1)?;
            }
        }
    }
    Ok(())
}

/// `schema` as strict tool use accepts it: the engine-only constraints are removed and described
/// in the property's `description` instead (the engine still enforces them).
pub fn for_strict_api(schema: &Value) -> Value {
    strip(schema, 0)
}

fn strip(schema: &Value, depth: usize) -> Value {
    let Some(obj) = schema.as_object() else { return schema.clone() };
    if depth > MAX_DEPTH {
        return schema.clone();
    }
    let mut out = Map::new();
    let mut notes = Vec::new();
    for (k, v) in obj {
        if ENGINE_ONLY.contains(&k.as_str()) {
            notes.push(format!("{k} {v}"));
            continue;
        }
        let v = match k.as_str() {
            "properties" => Value::Object(v.as_object().map(|p| p.iter().map(|(n, s)| (n.clone(), strip(s, depth + 1))).collect()).unwrap_or_default()),
            "items" => strip(v, depth + 1),
            _ => v.clone(),
        };
        out.insert(k.clone(), v);
    }
    if !notes.is_empty() {
        let d = out.get("description").and_then(Value::as_str).unwrap_or_default();
        let sep = if d.is_empty() { "" } else { " " };
        out.insert("description".into(), Value::String(format!("{d}{sep}({})", notes.join(", "))));
    }
    Value::Object(out)
}
