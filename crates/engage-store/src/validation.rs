//! Boundary checks shared by HTTP and in-process Board callers.
//! Ranges and interactions follow the pinned Board api/routes.ts schemas.
use engage_core::BoardError;
use serde_json::Value;

fn invalid(message: &str) -> BoardError {
    BoardError::invalid(message)
}
fn field<'a>(query: &'a [(String, String)], name: &str) -> Option<&'a str> {
    query
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}
fn number(
    query: &[(String, String)],
    name: &str,
    min: f64,
    max: f64,
    integer: bool,
) -> Result<(), BoardError> {
    let values: Vec<_> = query.iter().filter(|(k, _)| k == name).collect();
    if values.len() > 1 {
        return Err(invalid("numeric query fields cannot be repeated"));
    }
    if let Some((_, value)) = values.first() {
        let parsed = value
            .parse::<f64>()
            .map_err(|_| invalid("invalid numeric query field"))?;
        if !parsed.is_finite() || parsed < min || parsed > max || integer && parsed.fract() != 0.0 {
            return Err(invalid("numeric query field is out of range"));
        }
    }
    Ok(())
}
fn flag(query: &[(String, String)], name: &str) -> Result<(), BoardError> {
    for (_, value) in query.iter().filter(|(k, _)| k == name) {
        if !matches!(value.as_str(), "true" | "false" | "1" | "0") {
            return Err(invalid("a boolean query field must be true, false, 1 or 0"));
        }
    }
    Ok(())
}
fn string(body: &Value, name: &str, max: usize) -> Result<(), BoardError> {
    if body.get(name).is_some_and(|value| {
        value
            .as_str()
            .is_none_or(|s| s.len() > max || s.contains('\0'))
    }) {
        return Err(invalid("invalid string field"));
    }
    Ok(())
}
fn boolean(body: &Value, name: &str) -> Result<(), BoardError> {
    if body.get(name).is_some_and(|v| !v.is_boolean()) {
        return Err(invalid("invalid boolean field"));
    }
    Ok(())
}
fn array(body: &Value, name: &str) -> Result<(), BoardError> {
    if body.get(name).is_some_and(|v| !v.is_array()) {
        return Err(invalid("invalid list field"));
    }
    Ok(())
}
fn strict_query(query: &[(String, String)], names: &[&str]) -> Result<(), BoardError> {
    if query.iter().any(|(key, _)| !names.contains(&key.as_str())) {
        return Err(invalid("unknown query field"));
    }
    Ok(())
}

pub(crate) fn request(
    method: &str,
    path: &str,
    query: &[(String, String)],
    body: &Value,
) -> Result<(), BoardError> {
    if method == "GET" {
        for name in ["after", "re"] {
            let maximum = if path == "/channels" && name == "after" {
                1024
            } else {
                128
            };
            if field(query, name)
                .is_some_and(|s| s.is_empty() || s.len() > maximum || s.contains('\0'))
            {
                return Err(invalid("invalid reference"));
            }
        }
        if path == "/channels"
            || path == "/threads"
            || path == "/messages"
            || path.starts_with("/messages/")
            || path == "/tasks"
            || path.starts_with("/inbox/")
        {
            number(query, "limit", 1.0, 500.0, true)?;
        }
        match path {
            "/channels" => {
                strict_query(query, &["prefix", "after", "limit"])?;
                if field(query, "prefix").is_some_and(|s| s.is_empty() || s.len() > 2049) {
                    return Err(invalid("invalid channel prefix"));
                }
            }
            "/messages" => {
                strict_query(query, &["channel", "after", "limit"])?;
                if field(query, "channel").is_none_or(|s| !crate::messages::valid_channel(s)) {
                    return Err(invalid("a messages read needs one channel"));
                }
            }
            "/threads" => {
                if field(query, "before").is_some() && field(query, "after").is_some() {
                    return Err(invalid("before and after are mutually exclusive"));
                }
            }
            "/events" => {
                number(query, "wait", 0.0, 300.0, false)?;
                number(query, "last", 1.0, 100.0, true)?;
                flag(query, "all")?;
                if field(query, "last").is_some()
                    && (field(query, "after").is_some() || field(query, "wait").is_some())
                {
                    return Err(invalid("last takes neither after nor wait"));
                }
            }
            "/tasks" => {
                flag(query, "latest")?;
                if matches!(field(query, "latest"), Some("1" | "true"))
                    && field(query, "after").is_some()
                {
                    return Err(invalid("latest takes no after"));
                }
                if field(query, "state").is_some_and(|s| {
                    !matches!(
                        s,
                        "open"
                            | "all"
                            | "waiting"
                            | "ready"
                            | "claimed"
                            | "blocked"
                            | "completed"
                            | "failed"
                            | "canceled"
                    )
                }) {
                    return Err(invalid("invalid task state"));
                }
            }
            "/schedules" => {
                flag(query, "all")?;
            }
            "/watchers/matches" => {
                number(query, "limit", 1.0, 50.0, true)?;
                number(query, "wait", 0.0, 25.0, false)?;
            }
            _ => {}
        }
        if path == "/threads" || path == "/events" || path == "/tasks" {
            if field(query, "text").is_some_and(|s| s.is_empty() || s.len() > 200) {
                return Err(invalid("text filter is empty or too long"));
            }
            for (_, label) in query.iter().filter(|(k, _)| k == "label") {
                let key = label.split('=').next().unwrap_or("");
                if key.is_empty()
                    || !key.bytes().enumerate().all(|(i, b)| {
                        b.is_ascii_lowercase()
                            || b.is_ascii_digit()
                            || i > 0 && matches!(b, b'_' | b'.' | b'-')
                    })
                {
                    return Err(invalid("invalid label filter"));
                }
            }
        }
        return Ok(());
    }
    if method != "POST" && method != "PATCH" {
        return Ok(());
    }
    let content = path == "/messages" || path == "/tasks" || path == "/schedules";
    if content {
        if !body.is_object() {
            return Err(invalid("expected an object"));
        }
        for name in ["text", "channel", "re", "id", "after"] {
            string(
                body,
                name,
                if name == "text" {
                    65536
                } else if name == "channel" {
                    2048
                } else {
                    200
                },
            )?;
        }
        boolean(body, "noSync")?;
        array(body, "attachments")?;
        array(body, "sync")?;
        if body.get("labels").is_some_and(|labels| !labels.is_object()) {
            return Err(invalid("labels must be an object"));
        }
        if let Some(attachments) = body["attachments"].as_array() {
            if attachments.len() > 10 {
                return Err(invalid("at most 10 attachments per message"));
            }
            if attachments.iter().any(|v| v.as_str().is_none()) {
                return Err(invalid("attachments must name file IDs"));
            }
        }
    }
    if path.starts_with("/messages/") && path.ends_with("/edit") {
        string(body, "text", 65536)?;
        if body.get("text").is_none() {
            return Err(invalid("text is required"));
        }
    }
    if path.starts_with("/messages/")
        && (path.ends_with("/edit") || path.ends_with("/delete"))
        && body
            .get("revision")
            .is_some_and(|v| v.as_u64().is_none_or(|n| n == 0))
    {
        return Err(invalid("revision must be a positive integer"));
    }
    if path == "/tasks" || path.starts_with("/tasks/") {
        boolean(body, "claim")?;
        array(body, "needs")?;
        if body.get("lease").is_some_and(|v| {
            v.as_f64()
                .is_none_or(|n| !n.is_finite() || !(30.0..=3600.0).contains(&n))
        }) {
            return Err(invalid("lease must be 30 to 3600 seconds"));
        }
        if body
            .get("token")
            .is_some_and(|token| token.as_str().is_none_or(|s| s.is_empty() || s.len() > 128))
        {
            return Err(invalid("invalid claim token"));
        }
        if body
            .get("epoch")
            .is_some_and(|v| v.as_u64().is_none_or(|n| n == 0))
        {
            return Err(invalid("epoch must be a positive integer"));
        }
    }
    if path == "/schedules" {
        for name in ["in", "at", "every", "until", "for"] {
            string(body, name, 100)?;
        }
        if body
            .get("times")
            .is_some_and(|v| v.as_u64().is_none_or(|n| n == 0))
        {
            return Err(invalid("times must be a positive integer"));
        }
        if body.get("in").is_some() && body.get("at").is_some() {
            return Err(invalid("in and at are mutually exclusive"));
        }
        let stops = ["until", "for", "times"]
            .iter()
            .filter(|name| body.get(**name).is_some())
            .count();
        if stops > 1 || stops > 0 && body.get("every").is_none() {
            return Err(invalid("choose one stop for a repeating schedule"));
        }
    }
    Ok(())
}
