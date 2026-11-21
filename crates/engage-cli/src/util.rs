use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::args::{Filters, Options};

pub fn duration(value: &str) -> Result<f64> {
    let (number, multiplier) = match value.as_bytes().last() {
        Some(b's') => (&value[..value.len() - 1], 1.0),
        Some(b'm') => (&value[..value.len() - 1], 60.0),
        Some(b'h') => (&value[..value.len() - 1], 3600.0),
        Some(b'd') => (&value[..value.len() - 1], 86400.0),
        _ => (value, 1.0),
    };
    let number: f64 = number
        .parse()
        .context("durations look like 30s, 10m, 1h, or a number of seconds")?;
    if !number.is_finite() || number < 0.0 {
        bail!("a duration must be finite and nonnegative");
    }
    let seconds = number * multiplier;
    if !seconds.is_finite() {
        bail!("duration is too large");
    }
    Ok(seconds)
}

pub fn read_text(text: Option<&str>, file: Option<&Path>) -> Result<String> {
    if let Some(file) = file {
        if file != Path::new("-") {
            return std::fs::read_to_string(file)
                .with_context(|| format!("reading {}", file.display()));
        }
    } else if let Some(text) = text.filter(|s| *s != "-") {
        return Ok(text.into());
    }
    if file.is_none() && text.is_none() {
        bail!("provide text, --file PATH, or - for stdin");
    }
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    Ok(input)
}

pub fn body_input(options: &Options) -> Result<Value> {
    if let Some(data) = &options.data {
        return serde_json::from_str(data).context("--data is not valid JSON");
    }
    if let Some(file) = &options.file {
        return serde_json::from_str(&read_text(None, Some(file))?)
            .context("file is not valid JSON");
    }
    bail!("provide --data JSON or --file PATH (- reads stdin)")
}

pub fn pairs(values: &[String]) -> Result<Vec<(String, String)>> {
    values
        .iter()
        .map(|pair| {
            let (key, value) = pair.split_once('=').context("expected KEY=VALUE")?;
            if key.is_empty() {
                bail!("a key cannot be empty");
            }
            Ok((key.into(), value.into()))
        })
        .collect()
}

pub fn labels(values: &[String]) -> Result<Value> {
    let mut object = Map::new();
    for (key, value) in pairs(values)? {
        object.insert(key, Value::String(value));
    }
    Ok(Value::Object(object))
}

pub fn optional<T: Serialize>(body: &mut Value, key: &str, value: Option<&T>) {
    if let Some(value) = value {
        body[key] = serde_json::to_value(value).expect("CLI option serializes");
    }
}

pub fn filters(filters: &Filters) -> Vec<(String, String)> {
    let mut query = Vec::new();
    for (name, value) in [
        ("re", &filters.re),
        ("by", &filters.by),
        ("mentions", &filters.mentions),
        ("from", &filters.source),
        ("text", &filters.text),
        ("after", &filters.after),
        ("before", &filters.before),
    ] {
        if let Some(value) = value {
            query.push((name.into(), value.clone()));
        }
    }
    for label in &filters.label {
        query.push(("label".into(), label.clone()));
    }
    if let Some(limit) = filters.limit {
        query.push(("limit".into(), limit.to_string()));
    }
    if let Some(last) = filters.last {
        query.push(("last".into(), last.to_string()));
    }
    query
}

pub fn query_options(options: &Options) -> Vec<(String, String)> {
    let mut query = Vec::new();
    for (name, value) in [
        ("re", &options.re),
        ("after", &options.after),
        ("before", &options.before),
        ("by", &options.by),
        ("state", &options.state),
        ("owner", &options.owner),
        ("for", &options.for_value),
    ] {
        if let Some(value) = value {
            query.push((name.into(), value.clone()));
        }
    }
    for label in &options.label {
        query.push(("label".into(), label.clone()));
    }
    if let Some(limit) = options.limit {
        query.push(("limit".into(), limit.to_string()));
    }
    if options.all {
        query.push(("all".into(), "true".into()));
    }
    if options.latest {
        query.push(("latest".into(), "true".into()));
    }
    query
}

pub fn output(value: &Value, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(value)?);
        return Ok(());
    }
    if let Some(message) = value
        .get("message")
        .or_else(|| value.get("task"))
        .filter(|m| m.is_object())
    {
        println!(
            "#{}  {}",
            message["id"].as_str().unwrap_or("?"),
            message["text"].as_str().unwrap_or_default()
        );
        if let Some(replies) = value["replies"].as_array() {
            for reply in replies {
                println!(
                    "  #{}  {}",
                    reply["id"].as_str().unwrap_or("?"),
                    reply["text"].as_str().unwrap_or_default()
                );
            }
        }
        if let Some(cursor) = value["cursor"].as_str() {
            println!("cursor {cursor}");
        }
    } else if value.get("claim").is_some() {
        let mut redacted = value.clone();
        redacted
            .as_object_mut()
            .expect("claim response is an object")
            .remove("claim");
        println!("{}", serde_json::to_string_pretty(&redacted)?);
    } else {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durations_and_labels_preserve_meaning() -> Result<()> {
        assert_eq!(duration("1.5m")?, 90.0);
        assert_eq!(duration("30")?, 30.0);
        assert!(duration("NaN").is_err());
        assert!(duration("-3s").is_err());
        assert_eq!(
            labels(&["state=open=waiting".into()])?["state"],
            "open=waiting"
        );
        Ok(())
    }
}
