//! Grant resolution adapted from Vercel Board (Apache-2.0), copyright 2026 Vercel Inc.
//! Modified for SQL-backed Rust authorization and verified OIDC facts.
use std::collections::{BTreeMap, BTreeSet};

use engage_core::{BoardError, Principal};
use serde_json::{Map, Value, json};

use crate::db::Tx;

const PERMISSIONS: [&str; 6] = ["read", "post", "react", "label", "sync", "manage"];
type Rules = BTreeMap<String, BTreeSet<String>>;

fn forbidden() -> BoardError {
    BoardError::new(
        403,
        "forbidden",
        "this credential cannot perform that operation",
    )
}

fn invalid(message: &str) -> BoardError {
    BoardError::new(400, "invalid_grant", message)
}

mod rules;
pub(crate) use rules::scope_rules;
use rules::{decision, pattern_valid, resolve, rules, specificity};

/// Authorization always intersects a caller's narrowing scope, including owners.
pub(crate) async fn allowed(
    tx: &mut Tx<'_>,
    p: &Principal,
    channel: &str,
    permission: &str,
) -> Result<bool, BoardError> {
    let grants = tx.list("grant").await?;
    if channel == "*" {
        for point in sample_points(&grants, p.scope.as_deref())? {
            if !decision(&grants, p, &point)?["can"]
                .as_array()
                .is_some_and(|can| can.iter().any(|v| v == permission))
            {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    Ok(decision(&grants, p, channel)?["can"]
        .as_array()
        .is_some_and(|can| can.iter().any(|v| v == permission)))
}

/// Effective access summary, with the same narrowing used by mutations.
pub(crate) async fn summary(tx: &mut Tx<'_>, p: &Principal) -> Result<Value, BoardError> {
    let grants = tx.list("grant").await?;
    let mut patterns = BTreeSet::from(["*".to_owned()]);
    for grant in &grants {
        patterns.extend(rules(&grant["can"])?.into_keys());
    }
    if let Some(scope) = &p.scope {
        patterns.extend(scope_rules(scope)?.into_keys());
    }
    let mut outside = "engage-probe".to_owned();
    while patterns
        .iter()
        .any(|pat| pat != "*" && specificity(pat, &outside).is_some())
    {
        outside.push_str("-probe");
    }
    let mut result = Map::new();
    for pattern in &patterns {
        let point = if pattern == "*" {
            outside.clone()
        } else if pattern.ends_with('/') {
            let mut child = format!("{pattern}engage-probe");
            while patterns.contains(&child) || patterns.contains(&format!("{child}/")) {
                child.push_str("-probe");
            }
            child
        } else {
            pattern.clone()
        };
        let can = decision(&grants, p, &point)?["can"].clone();
        if pattern != "*" || can.as_array().is_some_and(|a| !a.is_empty()) {
            result.insert(pattern.clone(), can);
        }
    }
    Ok(Value::Object(result))
}

fn sample_points(grants: &[Value], scope: Option<&str>) -> Result<BTreeSet<String>, BoardError> {
    let mut patterns = BTreeSet::from(["*".to_owned()]);
    for grant in grants {
        patterns.extend(rules(&grant["can"])?.into_keys());
    }
    if let Some(scope) = scope {
        patterns.extend(scope_rules(scope)?.into_keys());
    }
    let mut points = BTreeSet::new();
    let mut outside = "engage-probe".to_owned();
    while patterns
        .iter()
        .any(|p| p != "*" && specificity(p, &outside).is_some())
    {
        outside.push_str("-probe");
    }
    for pattern in &patterns {
        let base = if pattern == "*" {
            outside.as_str()
        } else {
            pattern.trim_end_matches('/')
        };
        points.insert(base.to_owned());
        let mut child = format!("{base}/engage-probe");
        while patterns.contains(&child) || patterns.contains(&format!("{child}/")) {
            child.push_str("-probe");
        }
        points.insert(child);
    }
    Ok(points)
}

fn strict(value: &Value, fields: &[&str]) -> Result<(), BoardError> {
    let obj = value
        .as_object()
        .ok_or_else(|| invalid("expected an object"))?;
    if obj.keys().any(|key| !fields.contains(&key.as_str())) {
        return Err(invalid("unknown field"));
    }
    Ok(())
}

fn text(value: &Value, field: &str, max: usize) -> Result<String, BoardError> {
    value[field]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= max && !s.contains('\0'))
        .map(str::to_owned)
        .ok_or_else(|| invalid("invalid principal field"))
}

fn environments(value: &Value) -> Result<Value, BoardError> {
    if value.is_null() {
        return Ok(json!(["production"]));
    }
    let list = value
        .as_array()
        .filter(|items| !items.is_empty() && items.len() <= 16)
        .ok_or_else(|| invalid("invalid environments"))?;
    let mut out = Vec::new();
    for item in list {
        let name = item
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 100)
            .ok_or_else(|| invalid("invalid environment"))?;
        if !out.contains(&name) {
            out.push(name);
        }
    }
    Ok(json!(out))
}

fn normalize_subject(value: &Value, removal: bool) -> Result<Value, BoardError> {
    let kind = text(value, "kind", 32)?;
    match kind.as_str() {
        "user" => {
            strict(value, &["kind", "userId", "email"])?;
            if value.get("email").is_some() == value.get("userId").is_some() {
                return Err(invalid("a user needs email or userId"));
            }
            let id = if value.get("userId").is_some() {
                text(value, "userId", 200)?
            } else {
                text(value, "email", 320)?
            };
            Ok(json!({"kind":"user","userId":id,"email":value.get("email")}))
        }
        "project" => {
            strict(value, &["kind", "project", "environments"])?;
            Ok(
                json!({"kind":"project","projectId":text(value,"project",200)?,"name":null,"teamId":null,"environments":environments(&value["environments"])?}),
            )
        }
        "app" => {
            strict(value, &["kind", "app"])?;
            Ok(json!({"kind":"app","clientId":text(value,"app",200)?,"name":null}))
        }
        "github" => {
            strict(
                value,
                &["kind", "repository", "workflow", "branch", "event"],
            )?;
            let repository = text(value, "repository", 200)?;
            if repository.split('/').count() != 2
                || repository.starts_with('/')
                || repository.ends_with('/')
            {
                return Err(invalid("repository must be owner/name"));
            }
            let mut out = json!({"kind":"github","repository":repository,"repositoryId":null});
            for field in ["workflow", "branch", "event"] {
                if value.get(field).is_some() {
                    out[field] = json!(text(value, field, 300)?);
                }
            }
            Ok(out)
        }
        "oidc" => {
            strict(value, &["kind", "issuer", "claims"])?;
            let issuer = text(value, "issuer", 1000)?;
            let claims = value["claims"]
                .as_object()
                .filter(|m| !m.is_empty() && m.len() <= 32)
                .ok_or_else(|| invalid("claims must name at least one claim"))?;
            if !removal && !claims.contains_key("sub") {
                return Err(invalid("OIDC grants must name a subject claim"));
            }
            for (name, claim) in claims {
                if name.is_empty() || name.len() > 200 {
                    return Err(invalid("invalid claim name"));
                }
                let strings: Vec<&Value> = claim
                    .as_array()
                    .map_or_else(|| vec![claim], |items| items.iter().collect());
                if strings.is_empty()
                    || strings.len() > 32
                    || strings
                        .iter()
                        .any(|s| s.as_str().is_none_or(|s| s.is_empty() || s.len() > 1000))
                {
                    return Err(invalid("invalid claim value"));
                }
                if name == "sub"
                    && !removal
                    && strings.iter().filter_map(|v| v.as_str()).any(|subject| {
                        subject.contains('*') && rules::subject_prefix(subject).is_none()
                    })
                {
                    return Err(invalid(
                        "subject wildcard must follow a bounded : or / prefix",
                    ));
                }
            }
            Ok(json!({"kind":"oidc","issuer":issuer,"claims":claims}))
        }
        "all-projects" => {
            strict(value, &["kind", "environments"])?;
            Ok(json!({"kind":kind,"environments":environments(&value["environments"])?}))
        }
        "all-users" | "all-apps" | "everyone" => {
            strict(value, &["kind"])?;
            Ok(json!({"kind":kind}))
        }
        _ => Err(invalid("unknown principal kind")),
    }
}

fn same_subject(a: &Value, b: &Value) -> bool {
    if a["kind"] != b["kind"] {
        return false;
    }
    match a["kind"].as_str().unwrap_or("") {
        "project" => a["projectId"] == b["projectId"],
        "user" => a["userId"] == b["userId"],
        "app" => a["clientId"] == b["clientId"],
        "all-projects" => true,
        _ => a == b,
    }
}

fn join_subject(subjects: &mut Vec<Value>, new: Value) {
    if let Some(old) = subjects.iter_mut().find(|p| same_subject(p, &new)) {
        if let (Some(old), Some(add)) = (
            old["environments"].as_array_mut(),
            new["environments"].as_array(),
        ) {
            for env in add {
                if !old.contains(env) {
                    old.push(env.clone());
                }
            }
        }
    } else {
        subjects.push(new);
    }
}

fn build_grant(body: &Value, p: &Principal) -> Result<Value, BoardError> {
    strict(body, &["principals", "can", "lockMeOut"])?;
    rules(&body["can"])?;
    let input = body["principals"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 32)
        .ok_or_else(|| invalid("a grant needs 1 to 32 principals"))?;
    let mut principals = Vec::new();
    for value in input {
        join_subject(&mut principals, normalize_subject(value, false)?);
    }
    Ok(
        json!({"id":uuid::Uuid::new_v4().to_string(),"principals":principals,"can":body["can"],"capped":false,"by":p.actor(),"at":crate::now()}),
    )
}

async fn check_change(
    tx: &mut Tx<'_>,
    p: &Principal,
    old: &[Value],
    new: &[Value],
    lock_out: bool,
) -> Result<(), BoardError> {
    let current = tx.list("grant").await?;
    let mut future = current.clone();
    for grant in old {
        future.retain(|g| g["id"] != grant["id"]);
    }
    future.extend(new.iter().cloned());
    let mut probes = sample_points(&current, p.scope.as_deref())?;
    probes.extend(sample_points(new, p.scope.as_deref())?);
    for grant in old.iter().chain(new) {
        for pattern in rules(&grant["can"])?.keys() {
            // Test a subtree outside exact root exceptions; exact grants test only their channel.
            let point = if pattern == "*" {
                "engage-probe".to_owned()
            } else if pattern.ends_with('/') {
                format!("{}engage-probe", pattern)
            } else {
                pattern.clone()
            };
            if !allowed(tx, p, &point, "manage").await? {
                return Err(forbidden());
            }
            if !lock_out
                && !p.owner
                && !decision(&future, p, &point)?["can"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v == "manage"))
            {
                return Err(BoardError::conflict(
                    "grant_lockout",
                    "this change would remove your ability to revoke it",
                ));
            }
        }
    }
    // A narrowed credential cannot delegate permissions it does not have anywhere
    // inside a changed pattern, including exceptions below that pattern.
    for grant in new {
        let written = rules(&grant["can"])?;
        for point in &probes {
            if let Some(given) = resolve(&written, point) {
                let can = decision(&current, p, point)?;
                for permission in given {
                    if !can["can"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|v| v == permission))
                    {
                        return Err(forbidden());
                    }
                }
            }
        }
    }
    // Admission also exposes permissions from pre-existing everyone grants.
    if new.iter().any(|g| {
        g["principals"]
            .as_array()
            .is_some_and(|a| a.iter().any(|s| s["kind"] != "everyone"))
    }) {
        for grant in &future {
            if grant["principals"]
                .as_array()
                .is_some_and(|a| a.iter().any(|s| s["kind"] == "everyone"))
            {
                for point in &probes {
                    if let Some(given) = resolve(&rules(&grant["can"])?, point) {
                        let can = decision(&current, p, point)?;
                        if given.iter().any(|permission| {
                            !can["can"]
                                .as_array()
                                .is_some_and(|a| a.iter().any(|v| v == permission))
                        }) {
                            return Err(forbidden());
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn query<'a>(query: &'a [(String, String)], key: &str) -> Option<&'a str> {
    query
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn explaining(
    query_values: &[(String, String)],
    p: &Principal,
) -> Result<(Principal, bool), BoardError> {
    let names = ["user", "project", "github", "oidc", "app"];
    if names
        .iter()
        .filter(|k| query(query_values, k).is_some())
        .count()
        > 1
    {
        return Err(BoardError::invalid("name one principal"));
    }
    let mut who = p.clone();
    let mut explicit = false;
    for name in names {
        if let Some(id) = query(query_values, name) {
            explicit = true;
            who.owner = false;
            who.profile = None;
            who.scope = None;
            who.groups.clear();
            who.facts = Value::Null;
            match name {
                "user" => who.id = format!("local:user:{id}"),
                "project" => {
                    who.id = format!(
                        "vercel:project:{id}:{}",
                        query(query_values, "environment").unwrap_or("production")
                    )
                }
                "app" => who.id = format!("vercel:app:{id}"),
                "github" => {
                    who.id = format!("github:repo:{id}");
                    let mut claims = json!({"repository":id});
                    for (key, target) in [
                        ("workflow", "workflow_ref"),
                        ("branch", "ref"),
                        ("event", "event_name"),
                    ] {
                        if let Some(v) = query(query_values, key) {
                            claims[target] = json!(if key == "branch" {
                                format!("refs/heads/{v}")
                            } else if key == "workflow" {
                                format!(
                                    "{id}/{v}@refs/heads/{}",
                                    query(query_values, "branch").unwrap_or("main")
                                )
                            } else {
                                v.to_owned()
                            });
                        }
                    }
                    who.facts = json!({"issuer":"github","claims":claims});
                }
                "oidc" => {
                    who.id = format!("oidc:{id}:explain");
                    let mut claims = Map::new();
                    for (key, value) in query_values {
                        if key == "claim" {
                            let (k, v) = value
                                .split_once('=')
                                .ok_or_else(|| BoardError::invalid("claim needs name=value"))?;
                            claims.insert(k.into(), json!(v));
                        }
                    }
                    who.facts = json!({"issuer":"oidc","iss":id,"claims":claims});
                }
                _ => {}
            }
        }
    }
    Ok((who, explicit))
}

pub(crate) async fn route(
    tx: &mut Tx<'_>,
    p: &Principal,
    method: &str,
    path: &str,
    body: &Value,
    query_values: &[(String, String)],
) -> Result<Option<Value>, BoardError> {
    if !path.starts_with("/grants") {
        return Ok(None);
    }
    if method == "GET" && path == "/grants/explain" {
        let channel = query(query_values, "channel")
            .ok_or_else(|| BoardError::invalid("channel is required"))?;
        if !pattern_valid(channel) || channel == "*" || channel.ends_with('/') {
            return Err(BoardError::invalid("invalid channel"));
        }
        let (who, explicit) = explaining(query_values, p)?;
        let full = allowed(tx, p, "*", "manage").await?;
        if explicit && !full {
            return Err(forbidden());
        }
        let mut result = decision(&tx.list("grant").await?, &who, channel)?;
        if !full {
            result["grants"] = json!([]);
        }
        return Ok(Some(result));
    }
    if method == "GET" && path == "/grants" {
        if !allowed(tx, p, "*", "manage").await? {
            return Err(forbidden());
        }
        return Ok(Some(json!({"grants":tx.list("grant").await?})));
    }
    if method == "POST" && (path == "/grants" || path == "/grants/batch") {
        let inputs = if path.ends_with("/batch") {
            strict(body, &["grants", "lockMeOut"])?;
            body["grants"]
                .as_array()
                .filter(|a| !a.is_empty() && a.len() <= 16)
                .ok_or_else(|| invalid("a batch needs 1 to 16 grants"))?
                .clone()
        } else {
            vec![body.clone()]
        };
        let made: Vec<_> = inputs
            .iter()
            .map(|b| build_grant(b, p))
            .collect::<Result<_, _>>()?;
        check_change(tx, p, &[], &made, body["lockMeOut"] == true).await?;
        for grant in &made {
            tx.put(
                "grant",
                grant["id"].as_str().expect("grant ID generated"),
                grant,
            )
            .await?;
        }
        return Ok(Some(if path.ends_with("/batch") {
            json!({"grants":made})
        } else {
            json!({"grant":made[0]})
        }));
    }
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    if parts.len() < 2 {
        return Err(BoardError::missing("route_not_found"));
    }
    let id = parts[1];
    let existing = tx.get("grant", id).await?;
    if method == "POST" && parts.get(2) == Some(&"revoke") {
        if !body.is_null() {
            strict(body, &["lockMeOut"])?;
        }
        if let Some(old) = existing {
            check_change(tx, p, &[old], &[], body["lockMeOut"] == true).await?;
            tx.remove("grant", id).await?;
            return Ok(Some(json!({"revoked":true})));
        }
        if !allowed(tx, p, "*", "manage").await? {
            return Err(forbidden());
        }
        return Ok(Some(json!({"revoked":false})));
    }
    if method == "PATCH" && parts.len() == 2 {
        let old = existing.ok_or_else(|| BoardError::missing("grant_not_found"))?;
        strict(body, &["add", "remove", "can", "lockMeOut"])?;
        if ["add", "remove", "can"]
            .iter()
            .all(|k| body.get(k).is_none())
        {
            return Err(invalid("name a grant change"));
        }
        let mut next = old.clone();
        let mut subjects = old["principals"]
            .as_array()
            .expect("stored principals validated")
            .clone();
        if let Some(remove) = body.get("remove") {
            let list = remove
                .as_array()
                .filter(|a| !a.is_empty() && a.len() <= 32)
                .ok_or_else(|| invalid("invalid remove list"))?;
            for value in list {
                let normalized = normalize_subject(value, true)?;
                let before = subjects.len();
                subjects.retain(|s| !same_subject(s, &normalized));
                if subjects.len() == before {
                    return Err(BoardError::new(
                        400,
                        "not_in_grant",
                        "principal is not in this grant",
                    ));
                }
            }
        }
        if let Some(add) = body.get("add") {
            let list = add
                .as_array()
                .filter(|a| !a.is_empty() && a.len() <= 32)
                .ok_or_else(|| invalid("invalid add list"))?;
            for value in list {
                join_subject(&mut subjects, normalize_subject(value, false)?);
            }
        }
        if subjects.is_empty() {
            return Err(BoardError::new(
                400,
                "grant_empty",
                "a grant must name a principal",
            ));
        }
        if subjects.len() > 32 {
            return Err(invalid("too many principals"));
        }
        next["principals"] = json!(subjects);
        if let Some(can) = body.get("can") {
            rules(can)?;
            next["can"] = can.clone();
        }
        next["by"] = json!(p.actor());
        next["at"] = json!(crate::now());
        check_change(
            tx,
            p,
            &[old],
            std::slice::from_ref(&next),
            body["lockMeOut"] == true,
        )
        .await?;
        tx.put("grant", id, &next).await?;
        return Ok(Some(json!({"grant":next})));
    }
    Err(BoardError::missing("route_not_found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn implications_and_most_specific_scope() {
        let rules = scope_rules("*=all;work/=read;work/secret=").expect("valid scope");
        assert!(
            resolve(&rules, "work/a")
                .expect("work scope")
                .contains("read")
        );
        assert!(
            !resolve(&rules, "work/a")
                .expect("work scope")
                .contains("post")
        );
        assert!(
            resolve(&rules, "work/secret")
                .expect("secret scope")
                .is_empty()
        );
        assert!(scope_rules("work/*=read").is_err());
        assert!(scope_rules("*=read;*=post").is_err());
    }
    #[test]
    fn closest_principal_wins_and_ties_union() {
        let p = Principal {
            id: "vercel:project:web:production".into(),
            owner: false,
            ..Principal::default()
        };
        let project =
            normalize_subject(&json!({"kind":"project","project":"web"}), false).expect("project");
        let grants = vec![
            json!({"id":"a","principals":[project],"can":{"work/":"all"}}),
            json!({"id":"b","principals":[{"kind":"everyone"}],"can":{"work/secret/":[]}}),
        ];
        assert_eq!(
            decision(&grants, &p, "work/secret/thread").expect("decision")["can"],
            json!([])
        );
        let mut grants = grants;
        grants.push(json!({"id":"c","principals":[project],"can":{"work/secret/":["post"]}}));
        assert_eq!(
            decision(&grants, &p, "work/secret/thread").expect("decision")["can"],
            json!(["post"])
        );
        let outsider = Principal {
            id: "local:user:outsider".into(),
            owner: false,
            ..Principal::default()
        };
        assert_eq!(
            decision(&grants, &outsider, "work/secret/thread").expect("decision")["pattern"],
            Value::Null
        );
    }
    #[test]
    fn owner_is_still_narrowed() {
        let p = Principal {
            scope: Some("*=read".into()),
            ..Principal::default()
        };
        assert_eq!(
            decision(&[], &p, "work").expect("decision")["can"],
            json!(["read"])
        );
    }
}
