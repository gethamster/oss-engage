use aws_lc_rs::signature::KeyPair;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use engage_server::{AuthConfig, BlobConfig, OidcConfig, ServerConfig, router};
use engage_store::Board;
use http_body_util::BodyExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn oidc_checks_signature_issuer_audience_expiry_and_verified_identity() {
    let random = aws_lc_rs::rand::SystemRandom::new();
    let key = aws_lc_rs::signature::Ed25519KeyPair::generate_pkcs8(&random)
        .expect("ephemeral test key generates");
    let pair =
        aws_lc_rs::signature::Ed25519KeyPair::from_pkcs8(key.as_ref()).expect("test key parses");
    let public =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pair.public_key().as_ref());
    let jwks = json!({"keys":[{"kty":"OKP","crv":"Ed25519","x":public,"use":"sig","kid":"test","alg":"EdDSA"}]});
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("JWKS listener binds");
    let issuer = format!(
        "http://{}",
        listener.local_addr().expect("listener address exists")
    );
    let endpoint = axum::Router::new().route(
        "/keys",
        axum::routing::get(move || {
            let value = jwks.clone();
            async move { axum::Json(value) }
        }),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, endpoint)
            .await
            .expect("JWKS server runs");
    });
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("board opens");
    let app = router(
        board,
        ServerConfig {
            auth: AuthConfig::Oidc(OidcConfig {
                issuer: issuer.clone(),
                audience: "engage-test".into(),
                jwks_url: format!("{issuer}/keys"),
                owner_subjects: vec![],
            }),
            blobs: BlobConfig::Memory,
            ..Default::default()
        },
    )
    .await
    .expect("router fetches public identity keys");
    let expiry = chrono::Utc::now().timestamp() + 120;
    let claims = json!({"iss":issuer,"aud":"engage-test","sub":"verified-user","preferred_username":"verified","name":"Verified Person","exp":expiry});
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = Some("test".into());
    let signing = EncodingKey::from_ed_der(key.as_ref());
    let good = encode(&header, &claims, &signing).expect("test JWT signs");
    let request = |token: &str| {
        Request::builder()
            .uri("/me")
            .header("host", "127.0.0.1:4100")
            .header("authorization", format!("Bearer {token}"))
            .header("x-board-handle", "forged")
            .header("x-board-name", "Forged Person")
            .body(Body::empty())
            .expect("request builds")
    };
    let response = app
        .clone()
        .oneshot(request(&good))
        .await
        .expect("valid JWT dispatches");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response reads")
        .to_bytes();
    let me: Value = serde_json::from_slice(&bytes).expect("identity response parses");
    assert_eq!(me["handle"], "verified");
    assert_eq!(me["name"], "Verified Person");
    assert_eq!(me["can"], json!({}), "issuer groups do not grant ownership");
    for invalid in [
        json!({"aud":"wrong"}),
        json!({"iss":"https://forged.invalid"}),
        json!({"exp":expiry-600}),
    ] {
        let mut changed = claims.clone();
        for (name, value) in invalid.as_object().expect("overrides object") {
            changed[name] = value.clone();
        }
        let token = encode(&header, &changed, &signing).expect("invalid claims JWT signs");
        assert_eq!(
            app.clone()
                .oneshot(request(&token))
                .await
                .expect("request dispatches")
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let wrong = aws_lc_rs::signature::Ed25519KeyPair::generate_pkcs8(&random)
        .expect("another test key generates");
    let token = encode(&header, &claims, &EncodingKey::from_ed_der(wrong.as_ref()))
        .expect("wrong-key JWT signs");
    assert_eq!(
        app.clone()
            .oneshot(request(&token))
            .await
            .expect("request dispatches")
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(uuid::Uuid::new_v4().as_bytes()),
    )
    .expect("symmetric test JWT signs");
    assert_eq!(
        app.oneshot(request(&token))
            .await
            .expect("request dispatches")
            .status(),
        StatusCode::UNAUTHORIZED
    );
    task.abort();
}

#[tokio::test]
async fn real_verified_project_and_repository_tokens_match_only_their_grants() {
    let random = aws_lc_rs::rand::SystemRandom::new();
    let key = aws_lc_rs::signature::Ed25519KeyPair::generate_pkcs8(&random)
        .expect("ephemeral test key generates");
    let pair = aws_lc_rs::signature::Ed25519KeyPair::from_pkcs8(key.as_ref()).expect("key parses");
    let public =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pair.public_key().as_ref());
    let jwks = json!({"keys":[{"kty":"OKP","crv":"Ed25519","x":public,"use":"sig","kid":"test","alg":"EdDSA"}]});
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("JWKS server binds");
    let keys_url = format!(
        "http://{}/keys",
        listener.local_addr().expect("JWKS address exists")
    );
    let endpoint = axum::Router::new().route(
        "/keys",
        axum::routing::get(move || {
            let value = jwks.clone();
            async move { axum::Json(value) }
        }),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, endpoint)
            .await
            .expect("JWKS server runs");
    });
    let signing = EncodingKey::from_ed_der(key.as_ref());
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = Some("test".into());
    for (issuer, principal, extra, changed, expected_id) in [
        (
            "https://oidc.vercel.com/team_verified",
            json!({"kind":"project","project":"prj_verified","environments":["production"]}),
            json!({"project_id":"prj_verified","environment":"production","owner_id":"team_verified"}),
            json!({"environment":"preview"}),
            "vercel:project:prj_verified:production",
        ),
        (
            "https://token.actions.githubusercontent.com",
            json!({"kind":"github","repository":"gethamster/example","branch":"main"}),
            json!({"repository":"gethamster/example","repository_id":"123456","ref":"refs/heads/main","event_name":"push"}),
            json!({"ref":"refs/heads/untrusted"}),
            "github:repo:123456",
        ),
    ] {
        let board = Board::connect("sqlite::memory:")
            .await
            .expect("board opens");
        board
            .request(
                &engage_core::Principal::default(),
                "POST",
                "/grants",
                &[],
                json!({"principals":[principal],"can":{"deploy":["read","post"]}}),
            )
            .await
            .expect("specific grant creates");
        let app = router(
            board,
            ServerConfig {
                auth: AuthConfig::Oidc(OidcConfig {
                    issuer: issuer.into(),
                    audience: "engage-test".into(),
                    jwks_url: keys_url.clone(),
                    owner_subjects: vec![],
                }),
                blobs: BlobConfig::Memory,
                ..Default::default()
            },
        )
        .await
        .expect("OIDC router initializes");
        let mut claims = json!({"iss":issuer,"aud":"engage-test","sub":"verified-workload","exp":chrono::Utc::now().timestamp()+120});
        for (key, value) in extra.as_object().expect("claims object") {
            claims[key] = value.clone();
        }
        let token = encode(&header, &claims, &signing).expect("JWT signs");
        let request = |token: &str, method: &str, path: &str, body: Value| {
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "127.0.0.1:4100")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(body.to_string()))
                .expect("request builds")
        };
        let response = app
            .clone()
            .oneshot(request(&token, "GET", "/me", Value::Null))
            .await
            .expect("identity reads");
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("identity body reads")
            .to_bytes();
        let me: Value = serde_json::from_slice(&bytes).expect("identity parses");
        assert_eq!(
            me["principal"], expected_id,
            "verified workload identity is canonical"
        );
        let response = app
            .clone()
            .oneshot(request(
                &token,
                "POST",
                "/messages",
                json!({"channel":"deploy","text":"verified deployment"}),
            ))
            .await
            .expect("authorized message dispatches");
        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "grant admits the verified workload from {issuer}"
        );
        for (key, value) in changed.as_object().expect("overrides object") {
            claims[key] = value.clone();
        }
        let rejected = encode(&header, &claims, &signing).expect("changed claims sign");
        let response = app
            .oneshot(request(
                &rejected,
                "POST",
                "/messages",
                json!({"channel":"deploy","text":"wrong environment or branch"}),
            ))
            .await
            .expect("restricted message dispatches");
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "grant restrictions survive real JWT verification for {issuer}"
        );
    }
    task.abort();
}
