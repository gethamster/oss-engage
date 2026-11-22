//! Permission resolution adapted from Vercel Board, Apache-2.0.
use super::{PERMISSIONS, Rules, invalid};
use engage_core::{BoardError, Principal};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn pattern_valid(pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let path = pattern.strip_suffix('/').unwrap_or(pattern);
    !path.is_empty()
        && pattern.len() <= 2049
        && path.split('/').all(|part| {
            !part.is_empty()
                && part.as_bytes()[0].is_ascii_alphanumeric()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        })
}

pub(super) fn specificity(pattern: &str, channel: &str) -> Option<usize> {
    if pattern == "*" {
        return Some(0);
    }
    if let Some(root) = pattern.strip_suffix('/') {
        (channel == root || channel.starts_with(&format!("{root}/"))).then_some(root.len() + 1)
    } else {
        (pattern == channel).then_some(usize::MAX)
    }
}

pub(super) fn expand(value: &Value) -> Result<BTreeSet<String>, BoardError> {
    if value == "all" {
        return Ok(PERMISSIONS.iter().map(|s| (*s).into()).collect());
    }
    let list = value
        .as_array()
        .ok_or_else(|| invalid("permissions must be all or a list"))?;
    if list.len() > 8 {
        return Err(invalid("too many permissions"));
    }
    let mut out = BTreeSet::new();
    for item in list {
        let permission = item
            .as_str()
            .filter(|s| PERMISSIONS.contains(s))
            .ok_or_else(|| invalid("unknown permission"))?;
        out.insert(permission.to_owned());
        if permission == "manage" {
            out.extend(PERMISSIONS.iter().map(|s| (*s).to_owned()));
        }
        if ["react", "label", "sync"].contains(&permission) {
            out.insert("read".into());
        }
    }
    Ok(out)
}

pub(super) fn rules(value: &Value) -> Result<Rules, BoardError> {
    let map = value
        .as_object()
        .ok_or_else(|| invalid("can must map channel patterns to permissions"))?;
    if map.len() > 64 {
        return Err(invalid("too many channel patterns"));
    }
    map.iter()
        .map(|(pattern, grant)| {
            if !pattern_valid(pattern) {
                return Err(invalid("invalid channel pattern"));
            }
            Ok((pattern.clone(), expand(grant)?))
        })
        .collect()
}

pub(crate) fn scope_rules(scope: &str) -> Result<Rules, BoardError> {
    let bad = || {
        BoardError::new(
            400,
            "invalid_can",
            "scope must use pattern=permissions;pattern=permissions",
        )
    };
    if scope.len() > 4000 {
        return Err(bad());
    }
    let mut written = Map::new();
    for entry in scope.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (pattern, permissions) = entry.split_once('=').ok_or_else(bad)?;
        let pattern = pattern.trim();
        if written.contains_key(pattern) {
            return Err(bad());
        }
        let permissions = permissions.trim();
        let value = if permissions == "all" {
            json!("all")
        } else {
            json!(
                permissions
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            )
        };
        written.insert(pattern.into(), value);
    }
    if written.is_empty() {
        return Err(bad());
    }
    rules(&Value::Object(written)).map_err(|_| bad())
}

pub(super) fn resolve<'a>(rules: &'a Rules, channel: &str) -> Option<&'a BTreeSet<String>> {
    rules
        .iter()
        .filter_map(|(p, permissions)| specificity(p, channel).map(|s| (s, permissions)))
        .max_by_key(|(s, _)| *s)
        .map(|(_, p)| p)
}

pub(super) fn principal_kind(p: &Principal) -> &str {
    // Identity categories come from verified token facts before string parsing.
    // A generic issuer's subject must never masquerade as another token family.
    match p.facts["issuer"].as_str() {
        Some("oidc") => "oidc",
        Some("github") => "github",
        Some("vercel") if p.facts["claims"]["project_id"].is_string() => "project",
        _ => match p.id.split(':').nth(1) {
            Some("project") => "project",
            Some("repo") => "github",
            Some("app") => "app",
            _ => "user",
        },
    }
}

pub(super) fn principal_id(p: &Principal) -> String {
    p.id.split(':').skip(2).collect::<Vec<_>>().join(":")
}

/// A bounded terminal subject wildcard cannot include a neighboring project.
pub(super) fn subject_prefix(value: &str) -> Option<&str> {
    let prefix = value.strip_suffix('*')?;
    (prefix.len() > 1 && (prefix.ends_with(':') || prefix.ends_with('/')) && !prefix.contains('*'))
        .then_some(prefix)
}

fn claim_matches(name: &str, expected: &str, actual: &str) -> bool {
    if name == "sub" {
        subject_prefix(expected).map_or(expected == actual, |prefix| actual.starts_with(prefix))
    } else {
        expected == actual
    }
}

pub(super) fn matches_principal(subject: &Value, p: &Principal) -> Option<u8> {
    let kind = principal_kind(p);
    let claims = &p.facts["claims"];
    match subject["kind"].as_str()? {
        "everyone" => Some(2),
        "all-users" if kind == "user" && p.groups.iter().any(|g| g == "member") => Some(1),
        "all-projects" if kind == "project" && p.groups.iter().any(|g| g == "team-project") => {
            let environment = claims["environment"]
                .as_str()
                .or_else(|| p.id.split(':').nth(3))
                .unwrap_or("production");
            subject["environments"]
                .as_array()?
                .iter()
                .any(|v| v == environment)
                .then_some(1)
        }
        "all-apps" if kind == "app" && p.groups.iter().any(|g| g == "team-app") => Some(1),
        "user" if kind == "user" => {
            let id = subject["userId"].as_str()?;
            (id == p.id
                || id == principal_id(p)
                || subject["email"].as_str().is_some_and(|v| v == p.handle))
            .then_some(0)
        }
        "project" if kind == "project" => {
            let project = claims["project_id"]
                .as_str()
                .or_else(|| p.id.split(':').nth(2))?;
            let environment = claims["environment"]
                .as_str()
                .or_else(|| p.id.split(':').nth(3))
                .unwrap_or("production");
            (subject["projectId"] == project
                && subject["environments"]
                    .as_array()?
                    .iter()
                    .any(|v| v == environment)
                && (subject["teamId"].is_null()
                    || claims["owner_id"].is_null()
                    || subject["teamId"] == claims["owner_id"]))
                .then_some(0)
        }
        "github" if kind == "github" || p.facts["issuer"] == "github" => {
            let repository = claims["repository"].as_str()?;
            let same_repo = subject["repositoryId"]
                .as_str()
                .map_or(subject["repository"] == repository, |id| {
                    claims["repository_id"] == id
                });
            let workflow = subject["workflow"].as_str().is_none_or(|w| {
                claims["workflow_ref"]
                    == format!("{repository}/{w}@{}", claims["ref"].as_str().unwrap_or(""))
            });
            let branch = subject["branch"]
                .as_str()
                .is_none_or(|b| claims["ref"] == format!("refs/heads/{b}"));
            let event = subject["event"]
                .as_str()
                .is_none_or(|e| claims["event_name"] == e);
            (same_repo && workflow && branch && event).then_some(0)
        }
        "oidc" if kind == "oidc" => {
            if subject["issuer"] != p.facts["iss"] {
                return None;
            }
            subject["claims"]
                .as_object()?
                .iter()
                .all(|(name, expected)| {
                    let actual = &claims[name];
                    if actual.is_null() {
                        return false;
                    }
                    let text = actual
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| actual.to_string());
                    expected.as_str().map_or_else(
                        || {
                            expected.as_array().is_some_and(|items| {
                                items
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .any(|value| claim_matches(name, value, &text))
                            })
                        },
                        |value| claim_matches(name, value, &text),
                    )
                })
                .then_some(0)
        }
        "app" if kind == "app" => (subject["clientId"] == p.id.split(':').nth(2)?).then_some(0),
        _ => None,
    }
}

pub(super) fn decision(
    grants: &[Value],
    p: &Principal,
    channel: &str,
) -> Result<Value, BoardError> {
    let mut entries: BTreeMap<String, (u8, BTreeSet<String>, Vec<Value>)> = BTreeMap::new();
    let mut admitted = false;
    for grant in grants {
        let level = grant["principals"]
            .as_array()
            .and_then(|items| items.iter().filter_map(|v| matches_principal(v, p)).min());
        let Some(level) = level else {
            continue;
        };
        admitted |= level < 2;
        for (pattern, permissions) in rules(&grant["can"])? {
            let entry = entries
                .entry(pattern)
                .or_insert_with(|| (level, BTreeSet::new(), Vec::new()));
            if level < entry.0 {
                *entry = (level, BTreeSet::new(), Vec::new());
            }
            if level == entry.0 {
                entry.1.extend(permissions);
                entry.2.push(grant.clone());
            }
        }
    }
    let mut covering: Vec<_> = entries
        .iter()
        .filter_map(|(pattern, data)| {
            specificity(pattern, channel).map(|rank| (rank, pattern, data))
        })
        .collect();
    covering.sort_by_key(|(rank, _, _)| std::cmp::Reverse(*rank));
    let mut permissions = BTreeSet::new();
    let mut out = json!({"channel":channel,"pattern":null,"over":[],"level":null,"grants":[],"capped":null,"can":[]});
    if p.owner {
        permissions.extend(PERMISSIONS.iter().map(|s| (*s).to_owned()));
    } else if admitted && let Some((_, pattern, (level, allowed, sources))) = covering.first() {
        permissions = allowed.clone();
        out["pattern"] = json!(pattern);
        out["over"] = json!(
            covering
                .iter()
                .skip(1)
                .map(|(_, p, _)| *p)
                .collect::<Vec<_>>()
        );
        out["level"] = json!(["principal", "group", "everyone"][*level as usize]);
        out["grants"] = json!(sources);
    }
    if let Some(scope) = &p.scope {
        let scope = scope_rules(scope)?;
        permissions.retain(|permission| {
            resolve(&scope, channel).is_some_and(|set| set.contains(permission))
        });
    }
    out["can"] = json!(
        PERMISSIONS
            .iter()
            .filter(|s| permissions.contains(**s))
            .collect::<Vec<_>>()
    );
    Ok(out)
}
