use engage_sync::{
    Credentials, GitHub, Linear, Provider, ProviderPost, Registry, Secret, Slack,
    StaticCredentials, SyncError, TargetKind, WebhookRequest,
};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use std::collections::BTreeMap;

const SECRET: &str = "fixture-signing-secret";
const NOW: i64 = 1_800_000_000;
fn credentials(bot: Option<&str>) -> StaticCredentials {
    StaticCredentials(Credentials {
        access_token: Secret::new("fixture-token"),
        webhook_secret: Secret::new(SECRET),
        bot_user_id: bot.map(str::to_owned),
    })
}
fn hmac(body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).expect("fixture key is valid");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}
fn signed_slack(body: Value) -> WebhookRequest {
    let body = serde_json::to_vec(&body).expect("fixture serializes");
    let mut signed = format!("v0:{NOW}:").into_bytes();
    signed.extend_from_slice(&body);
    WebhookRequest {
        headers: BTreeMap::from([
            ("x-slack-request-timestamp".into(), NOW.to_string()),
            ("x-slack-signature".into(), format!("v0={}", hmac(&signed))),
        ]),
        body,
        now_unix: NOW,
    }
}
fn signed_github(event: &str, body: Value) -> WebhookRequest {
    let body = serde_json::to_vec(&body).expect("fixture serializes");
    WebhookRequest {
        headers: BTreeMap::from([
            ("x-github-event".into(), event.into()),
            ("x-github-delivery".into(), "delivery-1".into()),
            (
                "x-hub-signature-256".into(),
                format!("sha256={}", hmac(&body)),
            ),
        ]),
        body,
        now_unix: NOW,
    }
}
fn signed_linear(mut body: Value) -> WebhookRequest {
    body["webhookTimestamp"] = json!(NOW * 1000);
    let body = serde_json::to_vec(&body).expect("fixture serializes");
    WebhookRequest {
        headers: BTreeMap::from([
            ("linear-signature".into(), hmac(&body)),
            ("linear-delivery".into(), "linear-delivery-1".into()),
        ]),
        body,
        now_unix: NOW,
    }
}

#[tokio::test]
async fn slack_verifies_challenge_and_rejects_body_tampering() {
    let provider = Slack::new(credentials(None));
    let mut request = signed_slack(json!({"type":"url_verification","challenge":"proof"}));
    let answer = provider
        .receive("slack", &request)
        .await
        .expect("valid signature");
    assert_eq!(answer.body, json!({"challenge":"proof"}));
    assert!(answer.incoming.is_empty());
    request.body.push(b' ');
    assert!(matches!(
        provider.receive("slack", &request).await,
        Err(SyncError::Signature)
    ));
}
#[tokio::test]
async fn slack_rejects_replay_outside_timestamp_window() {
    let provider = Slack::new(credentials(None));
    let mut request = signed_slack(json!({"type":"url_verification","challenge":"proof"}));
    request.now_unix += 301;
    assert!(matches!(
        provider.receive("slack", &request).await,
        Err(SyncError::Stale)
    ));
}
#[tokio::test]
async fn slack_messages_preserve_thread_identity_and_have_stable_delivery_ids() {
    let provider = Slack::new(credentials(None));
    let request = signed_slack(
        json!({"type":"event_callback","event_id":"Ev123","event":{"type":"message","channel":"C012","user":"U123","ts":"1727.123","thread_ts":"1727.100","text":"Hi <@U234> <https://example.com|proof>"}}),
    );
    let first = provider
        .receive("team", &request)
        .await
        .expect("valid fixture");
    let second = provider
        .receive("team", &request)
        .await
        .expect("redelivery");
    assert_eq!(first.incoming, second.incoming);
    let event = &first.incoming[0];
    assert_eq!(event.id, "Ev123");
    assert_eq!(event.thread, "team:C012:1727.100");
    assert_eq!(event.event["author"]["id"], "slack:user:U123");
    assert_eq!(event.event["isRoot"], false);
    assert_eq!(
        event.event["text"],
        "Hi @[slack:user:U234] [proof](https://example.com)"
    );
}
#[tokio::test]
async fn slack_edits_and_deletions_target_original_message() {
    let provider = Slack::new(credentials(None));
    for (subtype, kind) in [
        ("message_changed", "edited"),
        ("message_deleted", "deleted"),
    ] {
        let message = json!({"user":"U1","ts":"1.2","thread_ts":"1.1","text":"changed"});
        let request = signed_slack(
            json!({"type":"event_callback","event_id":subtype,"event":{"type":"message","subtype":subtype,"channel":"C1","message":message,"previous_message":message}}),
        );
        let result = provider
            .receive("s", &request)
            .await
            .expect("valid fixture");
        assert_eq!(result.incoming[0].event["kind"], kind);
        assert_eq!(result.incoming[0].event["message"], "1.2");
        assert_eq!(result.incoming[0].thread, "s:C1:1.1");
    }
}
#[tokio::test]
async fn slack_ignores_own_bot_and_does_not_invent_mentions() {
    let provider = Slack::new(credentials(Some("U123")));
    let request = signed_slack(
        json!({"type":"event_callback","event_id":"Ev123","event":{"type":"message","channel":"C1","user":"U123","ts":"1.1","text":"hi"}}),
    );
    assert!(
        provider
            .receive("s", &request)
            .await
            .expect("valid fixture")
            .incoming
            .is_empty()
    );
    assert_eq!(
        provider.mention_by_id("slack:user:U456"),
        Some("<@U456>".into())
    );
    assert_eq!(provider.mention_by_id("slack:user:U456><!channel"), None);
    assert_eq!(provider.mention_by_id("slack:user:B123"), None);
}
#[tokio::test]
async fn github_is_inbound_only_by_default() {
    let provider = GitHub::new(credentials(None));
    let error = provider
        .post(
            "owner/repo:12",
            TargetKind::Thread,
            &ProviderPost {
                markdown: "not sent".into(),
                ..Default::default()
            },
        )
        .await
        .expect_err("inbound default");
    assert!(matches!(error, SyncError::Unsupported(_)));
    assert!(!provider.capabilities().starts_threads);
}
#[tokio::test]
async fn github_never_creates_issues_from_board_roots() {
    let provider = GitHub::new(credentials(None)).with_outbound(true);
    assert!(matches!(
        provider
            .post("owner/repo", TargetKind::Channel, &ProviderPost::default())
            .await,
        Err(SyncError::Unsupported(_))
    ));
}
#[tokio::test]
async fn github_signature_is_checked_before_parsing() {
    let provider = GitHub::new(credentials(None));
    let request = WebhookRequest {
        body: b"not-json".to_vec(),
        ..Default::default()
    };
    assert!(matches!(
        provider.receive("g", &request).await,
        Err(SyncError::Signature)
    ));
}
#[tokio::test]
async fn github_pr_lifecycle_reconciles_labels_and_assignees() {
    let provider = GitHub::new(credentials(None));
    let request = signed_github(
        "pull_request",
        json!({"action":"opened","repository":{"id":7,"full_name":"acme/web"},"sender":{"id":10,"login":"ada","type":"User"},"pull_request":{"number":12,"title":"Fix","html_url":"https://github.com/acme/web/pull/12","draft":true,"head":{"sha":"abc123"},"labels":[{"name":"Bug"}],"assignees":[{"login":"ada"}]}}),
    );
    let result = provider
        .receive("gh", &request)
        .await
        .expect("valid fixture");
    let event = &result.incoming[0].event;
    assert_eq!(event["thread"], "gh:acme/web:12");
    assert_eq!(event["channel"], "gh:acme/web:pulls");
    assert_eq!(event["labels"]["state"], "draft");
    assert_eq!(event["labels"]["head"], "abc123");
    assert_eq!(event["labels"]["tag.bug"], "Bug");
    assert_eq!(event["owns"], json!(["tag.", "assignee."]));
    assert_eq!(event["createRoot"], true);
}
#[tokio::test]
async fn github_issue_comments_and_review_comments_group_correctly() {
    let provider = GitHub::new(credentials(None));
    let request = signed_github(
        "issue_comment",
        json!({"action":"created","repository":{"full_name":"acme/web"},"issue":{"number":4},"comment":{"id":91,"body":"  Hello  ","user":{"id":10,"login":"ada"}}}),
    );
    let result = provider
        .receive("gh", &request)
        .await
        .expect("valid fixture");
    assert_eq!(result.incoming[0].thread, "gh:acme/web:issue:4");
    assert_eq!(result.incoming[0].event["message"], "91");
    assert_eq!(result.incoming[0].event["text"], "Hello");
    let request = signed_github(
        "pull_request_review_comment",
        json!({"action":"edited","repository":{"full_name":"acme/web"},"pull_request":{"number":4},"comment":{"id":92,"in_reply_to_id":90,"body":"review","path":"src/main.rs","line":12,"user":{"id":10,"login":"ada"}}}),
    );
    let result = provider
        .receive("gh", &request)
        .await
        .expect("valid fixture");
    assert_eq!(result.incoming[0].thread, "gh:acme/web:4");
    assert_eq!(result.incoming[0].event["subthread"], "gh:acme/web:4:rc:90");
    assert_eq!(result.incoming[0].event["kind"], "edited");
}
#[tokio::test]
async fn github_failed_ci_labels_each_matching_pr() {
    let provider = GitHub::new(credentials(None));
    let request = signed_github(
        "workflow_run",
        json!({"action":"completed","repository":{"id":7,"full_name":"acme/web"},"workflow_run":{"name":"Tests","path":".github/workflows/test.yml","head_sha":"abc","conclusion":"failure","html_url":"https://github.com/acme/web/actions/runs/1","pull_requests":[{"number":12,"head":{"sha":"abc"}},{"number":13,"head":{"sha":"def"}}]}}),
    );
    let result = provider
        .receive("gh", &request)
        .await
        .expect("valid fixture");
    assert_eq!(result.incoming.len(), 1);
    assert_eq!(
        result.incoming[0].event["labels"]["ci.7-12.test"],
        "failure"
    );
    assert_eq!(
        result.incoming[0].event["author"]["id"],
        "github:user:41898282"
    );
}
#[tokio::test]
async fn github_rejects_path_traversal_target() {
    let provider = GitHub::new(credentials(None));
    assert!(
        provider
            .resolve_target("owner/repo/../../secrets:12")
            .await
            .is_err()
    );
    assert!(
        provider
            .resolve_target("owner/repo:12:rc:40")
            .await
            .is_err()
    );
}
#[tokio::test]
async fn linear_issue_lifecycle_and_comment_identity_match_board() {
    let provider = Linear::new(credentials(None));
    let request = signed_linear(
        json!({"type":"Issue","action":"create","actor":{"id":"person","name":"Ada","type":"user"},"data":{"id":"uuid-1","identifier":"ENG-12","title":"Fix it","team":{"key":"ENG"},"state":{"type":"started"},"priorityLabel":"High","labels":[{"name":"Bug"}]}}),
    );
    let result = provider
        .receive("lin", &request)
        .await
        .expect("valid fixture");
    assert_eq!(result.incoming[0].thread, "lin:ENG:uuid-1");
    assert_eq!(result.incoming[0].event["labels"]["state"], "started");
    assert_eq!(result.incoming[0].event["labels"]["priority"], "high");
    assert_eq!(result.incoming[0].event["createRoot"], true);
    let request = signed_linear(
        json!({"type":"Comment","action":"update","url":"https://linear.app/team/issue/ENG-12/fix","data":{"id":"comment-2","parentId":"comment-1","issueId":"uuid-1","body":"  updated  ","user":{"id":"person","name":"Ada","type":"user"}}}),
    );
    let result = provider
        .receive("lin", &request)
        .await
        .expect("valid fixture");
    assert_eq!(result.incoming[0].thread, "lin:ENG:uuid-1");
    assert_eq!(
        result.incoming[0].event["subthread"],
        "lin:ENG:uuid-1:c:comment-1"
    );
    assert_eq!(result.incoming[0].event["kind"], "edited");
    assert_eq!(result.incoming[0].event["text"], "updated");
}
#[tokio::test]
async fn linear_rejects_signed_stale_webhooks_and_suppresses_bot_creation() {
    let provider = Linear::new(credentials(Some("bot")));
    let mut request = signed_linear(
        json!({"type":"Issue","action":"create","actor":{"id":"bot","type":"OauthClient"},"data":{"id":"uuid","identifier":"ENG-1","title":"Title"}}),
    );
    let result = provider
        .receive("lin", &request)
        .await
        .expect("signed fixture");
    assert_eq!(result.incoming[0].event["createRoot"], false);
    request.now_unix += 61;
    assert!(matches!(
        provider.receive("lin", &request).await,
        Err(SyncError::Stale)
    ));
}
#[test]
fn secrets_are_redacted_and_registry_records_principal() {
    let credentials = credentials(None);
    let printed = format!("{credentials:?}");
    assert!(!printed.contains("fixture-token"));
    assert!(!printed.contains(SECRET));
    let mut registry = Registry::new();
    registry
        .register("slack", "service:sync", Slack::new(credentials))
        .expect("valid registration");
    assert_eq!(registry.principal("slack"), Some("service:sync"));
    assert_eq!(
        registry.discovery(),
        vec![json!({"key":"slack","platform":"slack"})]
    );
    assert!(registry.provider("unknown").is_err());
}

#[tokio::test]
async fn slack_legacy_verification_token_is_not_persisted_or_debug_logged() {
    let provider = Slack::new(credentials(None));
    let request = signed_slack(
        json!({"type":"event_callback","token":"legacy-sensitive-token","event_id":"Ev1","event":{"type":"message","channel":"C1","user":"U1","ts":"1.0","text":"hello"}}),
    );
    assert!(!format!("{request:?}").contains("legacy-sensitive-token"));
    let received = provider
        .receive("slack", &request)
        .await
        .expect("valid fixture");
    assert!(received.incoming[0].raw.get("token").is_none());
}
