//! Profile header and scope validation adapted from Board's Apache-2.0 contract.
//! Copyright 2026 Vercel Inc. Modified for Rust boundary validation.

use engage_core::BoardError;

use crate::error;

pub(crate) struct Profile {
    pub name: String,
    pub avatar: Option<String>,
}

pub(crate) fn profile(value: &str) -> Result<Option<Profile>, BoardError> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    let mut parser = Parser {
        bytes: value.as_bytes(),
        at: 0,
    };
    parser.spaces();
    let (name, kind) = parser.bare()?;
    if !matches!(kind, Kind::String | Kind::Token)
        || name.len() > 32
        || !name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return Err(invalid_profile());
    }
    let mut avatar = None;
    while parser.peek() == Some(b';') {
        parser.at += 1;
        parser.spaces();
        let start = parser.at;
        if !parser
            .peek()
            .is_some_and(|c| c.is_ascii_lowercase() || c == b'*')
        {
            return Err(invalid_profile());
        }
        while parser
            .peek()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-.*".contains(&c))
        {
            parser.at += 1;
        }
        let key = &parser.bytes[start..parser.at];
        let (parameter, kind) = if parser.peek() == Some(b'=') {
            parser.at += 1;
            parser.bare()?
        } else {
            (String::new(), Kind::Other)
        };
        if key == b"avatar" {
            if !matches!(kind, Kind::String) {
                return Err(invalid_profile());
            }
            let url = url::Url::parse(&parameter).map_err(|_| invalid_profile())?;
            let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1"));
            if url.scheme() != "https" && !(url.scheme() == "http" && local) {
                return Err(invalid_profile());
            }
            avatar = Some(parameter);
        }
    }
    parser.spaces();
    if parser.at != parser.bytes.len() {
        return Err(invalid_profile());
    }
    Ok(Some(Profile { name, avatar }))
}

#[derive(Clone, Copy)]
enum Kind {
    String,
    Token,
    Other,
}
struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }
    fn spaces(&mut self) {
        while self.peek() == Some(b' ') {
            self.at += 1;
        }
    }
    fn bare(&mut self) -> Result<(String, Kind), BoardError> {
        if self.peek() == Some(b'"') {
            self.at += 1;
            let mut value = String::new();
            while let Some(c) = self.peek() {
                self.at += 1;
                match c {
                    b'"' => return Ok((value, Kind::String)),
                    b'\\' => {
                        let escaped = self
                            .peek()
                            .filter(|c| matches!(*c, b'"' | b'\\'))
                            .ok_or_else(invalid_profile)?;
                        self.at += 1;
                        value.push(escaped as char);
                    }
                    0x20..=0x7e => value.push(c as char),
                    _ => return Err(invalid_profile()),
                }
            }
            return Err(invalid_profile());
        }
        let start = self.at;
        if self
            .peek()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'*')
        {
            while self
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~:/".contains(&c))
            {
                self.at += 1;
            }
            return Ok((
                String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned(),
                Kind::Token,
            ));
        }
        while self.peek().is_some_and(|c| c != b';' && c != b' ') {
            self.at += 1;
        }
        let value =
            std::str::from_utf8(&self.bytes[start..self.at]).map_err(|_| invalid_profile())?;
        let number = value.strip_prefix('-').unwrap_or(value);
        let integer =
            !number.is_empty() && number.len() <= 15 && number.bytes().all(|c| c.is_ascii_digit());
        let decimal = number.split_once('.').is_some_and(|(a, b)| {
            !a.is_empty()
                && a.len() <= 12
                && !b.is_empty()
                && b.len() <= 3
                && a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit())
        });
        let binary = value
            .strip_prefix(':')
            .and_then(|s| s.strip_suffix(':'))
            .is_some_and(|s| {
                s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c))
            });
        if !integer && !decimal && !binary && !matches!(value, "?0" | "?1") {
            return Err(invalid_profile());
        }
        Ok((value.into(), Kind::Other))
    }
}

pub(crate) fn scope(value: &str) -> Result<(), BoardError> {
    let mut patterns = std::collections::BTreeSet::new();
    if value.len() > 4000 {
        return Err(invalid_scope());
    }
    for entry in value.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (pattern, permissions) = entry.split_once('=').ok_or_else(invalid_scope)?;
        let pattern = pattern.trim();
        let channel = pattern.trim_end_matches('/');
        let valid = pattern == "*"
            || !channel.is_empty()
                && pattern.len() <= 501
                && !pattern.ends_with("//")
                && channel.split('/').all(|segment| {
                    segment
                        .as_bytes()
                        .first()
                        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                        && segment.bytes().all(|c| {
                            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-')
                        })
                });
        if !valid || !patterns.insert(pattern) {
            return Err(invalid_scope());
        }
        let permissions = permissions.trim();
        if permissions != "all"
            && !permissions.is_empty()
            && !permissions
                .split(',')
                .map(str::trim)
                .all(|p| matches!(p, "read" | "post" | "react" | "label" | "sync" | "manage"))
        {
            return Err(invalid_scope());
        }
    }
    if patterns.is_empty() {
        return Err(invalid_scope());
    }
    Ok(())
}

fn invalid_profile() -> BoardError {
    error(
        400,
        "invalid_profile",
        "x-board-profile must name a profile and an optional quoted HTTPS avatar",
    )
}
fn invalid_scope() -> BoardError {
    error(
        400,
        "invalid_scope",
        "x-board-can must contain distinct channel patterns and valid permissions",
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn structured_profiles_preserve_names_and_validate_avatars() {
        let profile =
            super::profile("\"Codex\";avatar=\"https://example.test/codex.png\";extra=?1")
                .expect("structured header parses")
                .expect("profile exists");
        assert_eq!(profile.name, "Codex");
        assert_eq!(
            profile.avatar.as_deref(),
            Some("https://example.test/codex.png")
        );
        for value in [
            "1",
            "codex;avatar=https://example.test/avatar",
            "codex;avatar=\"javascript:alert(1)\"",
            "codex;avatar=\"http://remote.invalid/a\"",
            "codex trailing",
        ] {
            assert!(
                super::profile(value).is_err(),
                "reject invalid profile {value}"
            );
        }
    }

    #[test]
    fn scope_header_validates_even_on_discovery_and_empty_access() {
        for value in ["*=", "work/=read,post;private=read", "*=all"] {
            assert!(super::scope(value).is_ok(), "valid scope {value}");
        }
        for value in [
            "",
            "read",
            "*=read;*=post",
            "*=read,",
            "work//a=post",
            "*=all,read",
        ] {
            assert!(super::scope(value).is_err(), "invalid scope {value}");
        }
    }
}
