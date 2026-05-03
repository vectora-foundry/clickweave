use super::*;

pub(crate) fn redact_messages(messages: &[Message]) -> Result<Value> {
    let mut value = serde_json::to_value(messages)?;
    if let Value::Array(items) = &mut value {
        for item in items {
            if item.get("role").and_then(Value::as_str) == Some("system") {
                let content_sha = item.get("content").and_then(Value::as_str).map(prompt_sha);
                if let Value::Object(obj) = item {
                    obj.insert(
                        "content".to_string(),
                        Value::String("[SYSTEM_PROMPT_OMITTED]".to_string()),
                    );
                    if let Some(sha) = content_sha {
                        obj.insert("content_sha".to_string(), Value::String(sha));
                    }
                }
            }
        }
    }
    Ok(redact_value(value))
}

pub(crate) fn redact_tool_call(call: &ToolCall) -> ToolCallTrace {
    ToolCallTrace {
        name: call.function.name.clone(),
        arguments: redact_value(call.function.arguments.clone()),
    }
}

pub fn redact_value(value: Value) -> Value {
    match value {
        Value::String(s) => Value::String(redact_text(&s)),
        Value::Array(items) => Value::Array(items.into_iter().map(redact_value).collect()),
        Value::Object(obj) => {
            let mut redacted = Map::new();
            for (key, value) in obj {
                let lowered = key.to_lowercase();
                if lowered.contains("api_key")
                    || lowered.contains("authorization")
                    || lowered.contains("token")
                    || lowered.contains("secret")
                    || lowered.contains("password")
                {
                    redacted.insert(key, Value::String("[REDACTED_SECRET]".to_string()));
                } else if lowered == "image_url" || lowered.contains("base64") {
                    redacted.insert(key, Value::String("[IMAGE_OMITTED]".to_string()));
                } else {
                    redacted.insert(key, redact_value(value));
                }
            }
            Value::Object(redacted)
        }
        other => other,
    }
}

pub fn redact_text(input: &str) -> String {
    let mut out = input.to_string();
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        out = out.replace(&home, "[REDACTED_HOME]");
    }
    if contains_local_path(&out) {
        return "[REDACTED_PATH_CONTEXT]".to_string();
    }
    if looks_like_email(&out) || looks_like_phone(&out) {
        return "[REDACTED_PERSONAL_CONTEXT]".to_string();
    }
    if contains_http_url(&out) {
        return "[REDACTED_URL_CONTEXT]".to_string();
    }
    for marker in [
        "Bearer ",
        "api_key",
        "authorization",
        "private_key",
        "password",
    ] {
        if out.to_lowercase().contains(&marker.to_lowercase()) {
            out = "[REDACTED_SECRET_CONTEXT]".to_string();
            break;
        }
    }
    if out.starts_with("data:image/") {
        return "[IMAGE_OMITTED]".to_string();
    }
    const MAX_TEXT: usize = 6000;
    if out.len() > MAX_TEXT {
        out.truncate(MAX_TEXT);
        out.push_str("...[TRUNCATED]");
    }
    out
}

pub(crate) fn private_marker(input: &str) -> Option<&'static str> {
    let lowered = input.to_lowercase();
    for marker in [
        "/users/",
        "\\users\\",
        "/home/",
        "~/",
        "%appdata%",
        "application support",
        "api_key",
        "authorization",
        "private_key",
        "begin rsa",
        "begin openssh",
        "bearer ",
        "password",
    ] {
        if lowered.contains(marker) {
            return Some(marker);
        }
    }
    if lowered.contains("\"secret\"")
        || lowered.contains("secret_")
        || lowered.contains("\"token\"")
        || lowered.contains("token_")
    {
        return Some("secret");
    }
    personal_marker(input)
}

pub(crate) fn personal_marker(input: &str) -> Option<&'static str> {
    if contains_local_path(input) {
        return Some("path");
    }
    if looks_like_email(input) {
        return Some("email");
    }
    if looks_like_phone(input) {
        return Some("phone");
    }
    if contains_http_url(input) {
        return Some("url");
    }
    None
}

fn contains_local_path(input: &str) -> bool {
    let lowered = input.to_lowercase();
    lowered.contains("/users/")
        || lowered.contains("\\users\\")
        || lowered.contains("/home/")
        || lowered.contains("~/")
        || lowered.contains("%appdata%")
        || lowered.contains("application support")
}

fn contains_http_url(input: &str) -> bool {
    let lowered = input.to_lowercase();
    lowered.contains("http://") || lowered.contains("https://")
}

fn looks_like_email(input: &str) -> bool {
    input
        .split(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '"' | '\'' | '<' | '>' | ',' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}'
                )
        })
        .any(|token| {
            let Some((local, domain)) = token.split_once('@') else {
                return false;
            };
            !local.is_empty()
                && domain.contains('.')
                && domain
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        })
}

fn looks_like_phone(input: &str) -> bool {
    let mut digits = 0usize;
    let mut span = 0usize;
    for c in input.chars() {
        if c.is_ascii_digit() {
            digits += 1;
            span += 1;
        } else if matches!(c, '+' | '-' | '(' | ')' | '.' | ' ') && span > 0 {
            span += 1;
        } else {
            digits = 0;
            span = 0;
        }
        if digits >= 10 && span <= 30 {
            return true;
        }
        if span > 30 {
            digits = 0;
            span = 0;
        }
    }
    false
}

pub(crate) fn prompt_sha(prompt: &str) -> String {
    blake3::hash(prompt.as_bytes()).to_hex()[..16].to_string()
}
