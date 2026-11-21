//! Board tail syntax, including quoted text and repeated label filters.
use engage_core::BoardError;

pub(super) fn parse(expression: &str) -> Result<Vec<(String, String)>, BoardError> {
    let words = shlex::split(expression).ok_or_else(|| invalid("unterminated quote"))?;
    let mut query = vec![];
    let mut channel = None;
    let mut index = 0;
    while index < words.len() {
        let word = &words[index];
        index += 1;
        if word == "--" {
            for word in &words[index..] {
                if channel.replace(word.clone()).is_some() {
                    return Err(invalid("tail takes one channel"));
                }
            }
            break;
        }
        if !word.starts_with('-') {
            if channel.replace(word.clone()).is_some() {
                return Err(invalid("tail takes one channel"));
            }
            continue;
        }
        let flag = word
            .strip_prefix("--")
            .ok_or_else(|| invalid("unknown filter"))?;
        let (key, value) = flag
            .split_once('=')
            .map_or((flag, None), |(key, value)| (key, Some(value.to_string())));
        if key == "all" {
            if value.is_some() {
                return Err(invalid("all takes no value"));
            }
            query.push(("all".into(), "true".into()));
            continue;
        }
        if !matches!(key, "re" | "by" | "mentions" | "label" | "source" | "text") {
            return Err(invalid("unknown filter"));
        }
        let value = if let Some(value) = value {
            value
        } else if key == "mentions"
            && words
                .get(index)
                .is_none_or(|next| next.starts_with('-') || next != "me" && !next.contains('@'))
        {
            "me".to_string()
        } else {
            let value = words
                .get(index)
                .filter(|value| !value.starts_with('-'))
                .ok_or_else(|| invalid("filter needs a value"))?
                .clone();
            index += 1;
            value
        };
        if value.is_empty() {
            return Err(invalid("filter needs a value"));
        }
        if key == "mentions" && value == "me" {
            return Err(invalid("a watcher has no me; use an explicit mention"));
        }
        let (key, value) = match key {
            "source" => ("from", value),
            "re" => (key, value.trim_start_matches('#').to_string()),
            "by" | "mentions" => (key, value.trim_start_matches('@').to_string()),
            _ => (key, value),
        };
        query.push((key.into(), value));
    }
    if let Some(channel) = channel {
        query.insert(0, ("channel".into(), channel));
    }
    Ok(query)
}
fn invalid(message: &str) -> BoardError {
    BoardError::new(400, "invalid_watcher", message)
}

#[cfg(test)]
mod tests {
    use super::parse;
    #[test]
    fn quoted_filters_and_repeated_labels_match_cli_syntax() {
        assert_eq!(
            parse("work/ --text 'needs input' --label state=open --label owner --source slack")
                .expect("tail"),
            vec![
                ("channel".into(), "work/".into()),
                ("text".into(), "needs input".into()),
                ("label".into(), "state=open".into()),
                ("label".into(), "owner".into()),
                ("from".into(), "slack".into())
            ]
        );
        for tail in [
            "work --unknown x",
            "work other",
            "--mentions",
            "work --text 'unterminated",
            "work --all=true",
        ] {
            assert!(parse(tail).is_err(), "tail {tail}");
        }
    }
}
