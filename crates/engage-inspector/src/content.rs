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

/// Borrows one entry without copying a thread's full reply history.
pub fn thread_message<'a>(thread: &'a Value, id: &str) -> Option<&'a Value> {
    let root = message(thread);
    if field(root, "id") == id {
        return Some(root);
    }
    thread
        .get("replies")?
        .as_array()?
        .iter()
        .find(|reply| field(reply, "id") == id)
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

/// Uses each collection's protocol continuation without assuming numeric cursors.
pub fn page_cursor(value: &Value, key: &str, limit: usize) -> String {
    if key == "replies" {
        let replies = list(value, key);
        if replies.len() == limit {
            replies
                .last()
                .map(|reply| field(reply, "id").to_owned())
                .unwrap_or_default()
        } else {
            String::new()
        }
    } else if key == "tasks" {
        if list(value, key).len() == limit {
            field(value, "cursor").to_owned()
        } else {
            String::new()
        }
    } else {
        field(value, "older").to_owned()
    }
}

/// Combines overlapping collection pages while preserving the first page's order.
pub fn merge_items(items: Vec<Value>, page: Vec<Value>) -> Vec<Value> {
    let mut seen = std::collections::BTreeSet::new();
    items
        .into_iter()
        .chain(page)
        .filter(|item| seen.insert(field(message(item), "id").to_owned()))
        .collect()
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
    fn task_and_thread_pages_keep_distinct_continuation_contracts() {
        let first = json!({"tasks":[{"message":{"id":"a"}},{"message":{"id":"b"}}],"cursor":"opaque-task-cursor"});
        let next = json!({"tasks":[{"message":{"id":"b"}},{"message":{"id":"c"}}],"cursor":"next"});
        assert_eq!(page_cursor(&first, "tasks", 2), "opaque-task-cursor");
        assert_eq!(page_cursor(&first, "tasks", 3), "");
        assert_eq!(
            page_cursor(&json!({"tasks":[],"cursor":"end"}), "tasks", 2),
            ""
        );
        assert_eq!(
            page_cursor(
                &json!({"threads":[],"cursor":"feed","older":"older-page"}),
                "threads",
                2
            ),
            "older-page"
        );
        let combined = merge_items(list(&first, "tasks"), list(&next, "tasks"));
        let ids: Vec<_> = combined
            .iter()
            .map(|item| field(message(item), "id"))
            .collect();
        assert_eq!(ids, ["a", "b", "c"]);
    }

    #[test]
    fn forward_reply_pages_preserve_order_and_remove_page_overlap() {
        let current = json!({"replies":[{"id":"middle"},{"id":"latest"}]});
        let older = json!({"replies":[{"id":"oldest"},{"id":"middle"}]});
        assert_eq!(page_cursor(&current, "replies", 2), "latest");
        assert_eq!(page_cursor(&older, "replies", 3), "");
        let replies = merge_items(list(&older, "replies"), list(&current, "replies"));
        let ids: Vec<_> = replies.iter().map(|reply| field(reply, "id")).collect();
        assert_eq!(ids, ["oldest", "middle", "latest"]);
    }

    #[test]
    fn thread_lookup_borrows_the_root_or_exact_reply_and_reports_absence() {
        let thread = json!({"message":{"id":"root","text":"Root"},"replies":[{"id":"first","text":"First"},{"id":"last","text":"Last"}]});
        assert!(std::ptr::eq(
            thread_message(&thread, "root").expect("root"),
            &thread["message"]
        ));
        assert!(std::ptr::eq(
            thread_message(&thread, "last").expect("reply"),
            &thread["replies"][1]
        ));
        assert!(thread_message(&thread, "missing").is_none());
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
