//! Rendering and wire helpers shared by console views.

use serde_json::Value;

/// Renders Markdown while removing executable HTML and unsafe link protocols.
pub fn markdown(text: &str) -> String {
    let options = pulldown_cmark::Options::ENABLE_TABLES
        | pulldown_cmark::Options::ENABLE_STRIKETHROUGH
        | pulldown_cmark::Options::ENABLE_TASKLISTS;
    let mut rendered = String::new();
    pulldown_cmark::html::push_html(
        &mut rendered,
        pulldown_cmark::Parser::new_ext(text, options),
    );
    ammonia::Builder::default()
        .link_rel(Some("noopener noreferrer"))
        .clean(&rendered)
        .to_string()
}

/// A string-valued property, empty if an optional field is absent.
pub fn field<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or_default()
}

/// Accepts named list responses and array responses from Board endpoints.
pub fn list(value: &Value, name: &str) -> Vec<Value> {
    value
        .get(name)
        .unwrap_or(value)
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Returns the message embedded in a thread summary, or a message itself.
pub fn message(value: &Value) -> &Value {
    value.get("message").unwrap_or(value)
}

/// A root message's task state, if it is a task.
pub fn task_state(value: &Value) -> &str {
    value
        .get("task")
        .and_then(|task| task.get("state"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// A human-readable name without trusting it as an authenticated identity.
pub fn author(value: &Value) -> String {
    let actor = value.get("author").unwrap_or(&Value::Null);
    let name = field(actor, "name");
    if !name.is_empty() {
        name.to_owned()
    } else {
        let by = value.get("by").unwrap_or(&Value::Null);
        let name = field(by, "actor");
        if name.is_empty() {
            "Unknown author".into()
        } else {
            name.into()
        }
    }
}

/// A short accessible title for a thread row.
pub fn title(text: &str) -> String {
    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("Attachment");
    first
        .trim()
        .trim_start_matches('#')
        .trim()
        .chars()
        .take(160)
        .collect()
}

/// Keeps list rows current when edits, replies, or schedule ticks change their view.
pub fn row_key(value: &Value) -> String {
    let message = message(value);
    format!(
        "{}:{}:{}:{}:{}:{}:{}",
        field(message, "id"),
        message["version"],
        value["replies"],
        value["last"]["id"],
        field(value, "cursor"),
        field(value, "state"),
        field(value, "next")
    )
}

/// Upload tickets use the authenticated board origin, including through CLI proxies.
pub fn upload_url(origin: &str, target: &str) -> Result<String, &'static str> {
    let base = url::Url::parse(origin).map_err(|_| "Cannot determine this board's origin.")?;
    let resolved = base
        .join(target)
        .map_err(|_| "Engage returned an invalid upload endpoint.")?;
    if base.origin() != resolved.origin()
        || !resolved.username().is_empty()
        || resolved.password().is_some()
    {
        return Err(
            "The upload endpoint must use this board's origin. Reopen the inspector and try again.",
        );
    }
    Ok(resolved.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn markdown_removes_scripts_event_handlers_and_javascript_links() {
        let html = markdown(
            "<script>alert(1)</script><img src=x onerror=alert(1)>\n\n[click](javascript:alert(1))",
        );
        assert!(!html.contains("<script"));
        assert!(!html.contains("onerror"));
        assert!(!html.contains("javascript:"));
    }

    #[test]
    fn markdown_keeps_proof_code_and_safe_links() {
        let html = markdown(
            "**Verified** [commit](https://example.com/commit)\n\n```rust\nlet x = 2;\n```",
        );
        assert!(html.contains("<strong>Verified</strong>"));
        assert!(html.contains("href=\"https://example.com/commit\""));
        assert!(html.contains("let x = 2;"));
        assert!(html.contains("noopener noreferrer"));
    }

    #[test]
    fn summaries_and_arrays_use_the_same_message_view() {
        let value = json!({"threads":[{"message":{"id":"opaque/id","text":"# A task","task":{"state":"ready"}}}]});
        let values = list(&value, "threads");
        assert_eq!(field(message(&values[0]), "id"), "opaque/id");
        assert_eq!(title(field(message(&values[0]), "text")), "A task");
        assert_eq!(task_state(message(&values[0])), "ready");
        assert_eq!(list(&json!([1, 2]), "anything").len(), 2);
        assert_eq!(author(&json!({"author":{"name":"Alex"}})), "Alex");
        assert_eq!(author(&json!({"by":{"actor":"agent:adam"}})), "agent:adam");
        assert_ne!(
            row_key(&json!({"id":"1","version":1})),
            row_key(&json!({"id":"1","version":2}))
        );
    }

    #[test]
    fn upload_tickets_cannot_bypass_the_authenticated_origin() {
        let origin = "https://board.example";
        assert_eq!(
            upload_url(origin, "/attachments/uploads/id/data?ticket=proof")
                .expect("same-origin path"),
            "https://board.example/attachments/uploads/id/data?ticket=proof"
        );
        for target in [
            "https://other.example/upload",
            "http://board.example/upload",
            "https://board.example:444/upload",
            "//other.example/upload",
            "https://user:secret@board.example/upload",
        ] {
            assert!(
                upload_url(origin, target).is_err(),
                "cross-origin or credential-bearing ticket {target}"
            );
        }
    }
}
