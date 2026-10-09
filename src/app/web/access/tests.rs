use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{EncodingKey, Header, encode};
use rsa::{RsaPrivateKey, pkcs1::EncodeRsaPrivateKey, traits::PublicKeyParts};
use serde_json::Value;
use std::sync::OnceLock;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const SUBJECT: &str = "2f102676-72cd-4d60-9f49-a94a33f9b231";
const IDP: &str = "f43dbf29-6f6b-4f38-ac06-d8a973fb4e47";
fn key() -> &'static RsaPrivateKey {
    static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
    KEY.get_or_init(|| RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap())
}
pub(crate) fn config() -> Config {
    Config {
        issuer: "https://synthetic.cloudflareaccess.com".into(),
        audience: "a".repeat(64),
        account_id: "b".repeat(32),
        github_idp_id: IDP.into(),
    }
}
fn jwks(kid: &str) -> Value {
    json!({"keys":[{"kty":"RSA","alg":"RS256","use":"sig","kid":kid,
        "n":URL_SAFE_NO_PAD.encode(key().n().to_bytes_be()), "e":URL_SAFE_NO_PAD.encode(key().e().to_bytes_be())}]})
}
pub(crate) fn claims(subject: &str) -> Value {
    json!({"aud":[config().audience],"iss":config().issuer,"exp":now()+3600,"iat":now()-1,"nbf":now()-1,
        "sub":subject,"type":"app","identity_nonce":"synthetic-nonce","email":"synthetic@example.test"})
}
pub(crate) fn token(claims: &Value, kid: &str) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.into());
    encode(
        &header,
        claims,
        &EncodingKey::from_rsa_der(key().to_pkcs1_der().unwrap().as_bytes()),
    )
    .unwrap()
}
pub(crate) fn identity(subject: &str, id: &str) -> Value {
    json!({"id":id,"user_uuid":subject,"account_id":config().account_id,"idp":{"id":IDP,"type":"github"}})
}
pub(crate) async fn mock_verifier() -> (Verifier, MockServer) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/cdn-cgi/access/certs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks("synthetic-key")))
        .mount(&server)
        .await;
    let mut verifier = Verifier::new(config()).unwrap();
    // Test-only overrides; production endpoints always derive from the validated issuer.
    verifier.keys_url = format!("{}/cdn-cgi/access/certs", server.uri());
    verifier.identity_url = format!("{}/cdn-cgi/access/get-identity", server.uri());
    (verifier, server)
}
pub(crate) fn headers(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("cf-access-jwt-assertion", token.parse().unwrap());
    headers
}
async fn mount_identity(server: &MockServer, value: Value) {
    Mock::given(method("GET"))
        .and(path("/cdn-cgi/access/get-identity"))
        .respond_with(ResponseTemplate::new(200).set_body_json(value))
        .mount(server)
        .await;
}

#[tokio::test]
async fn signed_jwt_and_pinned_identity_are_both_required() {
    let (verifier, server) = mock_verifier().await;
    mount_identity(&server, identity(SUBJECT, "123456")).await;
    let value = token(&claims(SUBJECT), "synthetic-key");
    let verified = verifier.verify(&headers(&value)).await.unwrap();
    assert_eq!(verified.binding.provider_user_id, "123456");
    assert_eq!(verified.subject, SUBJECT);
    verifier.verify(&headers(&value)).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2); // keys and one identity lookup, exact-token cache
    assert_eq!(
        requests[1].headers["cookie"],
        format!("CF_Authorization={value}")
    );
    assert!(!requests[1].headers.contains_key("authorization"));
    let mut forged = HeaderMap::new();
    forged.insert(
        "cf-access-authenticated-user-email",
        "synthetic@example.test".parse().unwrap(),
    );
    assert!(matches!(
        verifier.verify(&forged).await,
        Err(StatusCode::UNAUTHORIZED)
    ));
    let mut duplicate = headers(&value);
    duplicate.append("cf-access-jwt-assertion", value.parse().unwrap());
    assert!(matches!(
        verifier.verify(&duplicate).await,
        Err(StatusCode::UNAUTHORIZED)
    ));
}

#[tokio::test]
async fn invalid_signature_issuer_audience_time_type_and_service_tokens_are_rejected() {
    let (verifier, server) = mock_verifier().await;
    mount_identity(&server, identity(SUBJECT, "123456")).await;
    for (field, value) in [
        ("iss", json!("https://evil.cloudflareaccess.com")),
        ("aud", json!(["wrong"])),
        ("exp", json!(now() - 1)),
        ("nbf", json!(now() + 60)),
        ("iat", json!(now() + 60)),
        ("type", json!("org")),
        ("sub", json!("")),
        ("identity_nonce", json!("")),
    ] {
        let mut input = claims(SUBJECT);
        input[field] = value;
        assert!(
            matches!(
                verifier
                    .verify(&headers(&token(&input, "synthetic-key")))
                    .await,
                Err(StatusCode::UNAUTHORIZED)
            ),
            "{field}"
        );
    }
    for field in ["exp", "iss", "aud", "sub", "nbf", "iat", "identity_nonce"] {
        let mut input = claims(SUBJECT);
        input.as_object_mut().unwrap().remove(field);
        assert!(
            matches!(
                verifier
                    .verify(&headers(&token(&input, "synthetic-key")))
                    .await,
                Err(StatusCode::UNAUTHORIZED)
            ),
            "missing {field}"
        );
    }
    let mut bad = token(&claims(SUBJECT), "synthetic-key");
    let end = bad.rfind('.').unwrap() + 1;
    bad.replace_range(
        end..end + 1,
        if &bad[end..end + 1] == "A" { "B" } else { "A" },
    );
    assert!(matches!(
        verifier.verify(&headers(&bad)).await,
        Err(StatusCode::UNAUTHORIZED)
    ));
    let hs = encode(
        &Header::new(Algorithm::HS256),
        &claims(SUBJECT),
        &EncodingKey::from_secret(b"synthetic"),
    )
    .unwrap();
    assert!(matches!(
        verifier.verify(&headers(&hs)).await,
        Err(StatusCode::UNAUTHORIZED)
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn identity_mismatch_wrong_account_or_idp_and_email_subject_are_rejected() {
    for (field, value) in [
        ("user_uuid", json!("14571052-a5c8-4d5b-9576-c511ca92be69")),
        ("account_id", json!("c".repeat(32))),
        ("id", json!("synthetic@example.test")),
        ("idp", json!({"id":IDP,"type":"cloudflare"})),
        (
            "idp",
            json!({"id":"14571052-a5c8-4d5b-9576-c511ca92be69","type":"github"}),
        ),
    ] {
        let (verifier, server) = mock_verifier().await;
        let mut input = identity(SUBJECT, "123456");
        input[field] = value;
        mount_identity(&server, input).await;
        assert!(matches!(
            verifier
                .verify(&headers(&token(&claims(SUBJECT), "synthetic-key")))
                .await,
            Err(StatusCode::UNAUTHORIZED)
        ));
    }
}

#[tokio::test]
async fn unknown_keys_are_rate_limited_rotation_works_and_stale_keys_fail_closed() {
    let (verifier, server) = mock_verifier().await;
    mount_identity(&server, identity(SUBJECT, "123456")).await;
    let good = token(&claims(SUBJECT), "synthetic-key");
    verifier.verify(&headers(&good)).await.unwrap();
    for _ in 0..20 {
        assert!(matches!(
            verifier
                .verify(&headers(&token(&claims(SUBJECT), "unknown")))
                .await,
            Err(StatusCode::UNAUTHORIZED)
        ));
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/cdn-cgi/access/certs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks("rotated")))
        .mount(&server)
        .await;
    mount_identity(&server, identity(SUBJECT, "123456")).await;
    verifier.keys.lock().await.attempted = Some(Instant::now() - Duration::from_secs(31));
    verifier
        .verify(&headers(&token(&claims(SUBJECT), "rotated")))
        .await
        .unwrap();
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    {
        let mut keys = verifier.keys.lock().await;
        keys.refreshed = Some(Instant::now() - Duration::from_secs(301));
        keys.attempted = Some(Instant::now() - Duration::from_secs(31));
    }
    let rotated = token(&claims(SUBJECT), "rotated");
    for _ in 0..2 {
        assert!(matches!(
            verifier.verify(&headers(&rotated)).await,
            Err(StatusCode::SERVICE_UNAVAILABLE)
        ));
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn identity_redirects_and_unavailable_or_oversized_responses_fail_closed() {
    for response in [
        ResponseTemplate::new(302).insert_header("location", "https://evil.example"),
        ResponseTemplate::new(503),
        ResponseTemplate::new(200).set_body_string("x".repeat(65_537)),
    ] {
        let (verifier, server) = mock_verifier().await;
        Mock::given(method("GET"))
            .and(path("/cdn-cgi/access/get-identity"))
            .respond_with(response)
            .mount(&server)
            .await;
        assert!(matches!(
            verifier
                .verify(&headers(&token(&claims(SUBJECT), "synthetic-key")))
                .await,
            Err(StatusCode::SERVICE_UNAVAILABLE)
        ));
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}
#[test]
fn issuer_validation_pins_official_https_endpoints() {
    for issuer in [
        "http://synthetic.cloudflareaccess.com",
        "https://synthetic.cloudflareaccess.com/",
        "https://synthetic.cloudflareaccess.com.evil.test",
        "https://evil.test",
        "https://synthetic.cloudflareaccess.com/path",
        "https://user@synthetic.cloudflareaccess.com",
        "https://synthetic.cloudflareaccess.com:8443",
        "https://a.b.cloudflareaccess.com",
    ] {
        let mut value = config();
        value.issuer = issuer.into();
        assert!(value.validate().is_err(), "{issuer}");
    }
}
