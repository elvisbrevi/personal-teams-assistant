use crate::{
    config::Config,
    security::{Vault, constant_eq, random_secret},
    state::Store,
};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

#[async_trait]
pub trait AccessToken: Send + Sync {
    async fn access_token(&self) -> Result<String>;
    async fn invalidate(&self) {}
}
#[derive(Serialize, Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: String,
    expires_at: i64,
}
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: i64,
}
struct Pending {
    verifier: String,
    redirect_uri: String,
    expires_at: i64,
}
pub struct OAuth {
    config: Arc<Config>,
    client: reqwest::Client,
    client_secret: Option<String>,
    token_endpoint: String,
    me_endpoint: String,
    store: Arc<Store>,
    vault: Vault,
    tokens: Mutex<Option<Tokens>>,
    pending: Mutex<HashMap<String, Pending>>,
}
impl OAuth {
    pub fn new(
        config: Arc<Config>,
        client: reqwest::Client,
        client_secret: String,
        store: Arc<Store>,
        vault: Vault,
    ) -> Result<Self> {
        Self::new_with_secret(config, client, Some(client_secret), store, vault)
    }

    pub fn new_public(
        config: Arc<Config>,
        client: reqwest::Client,
        store: Arc<Store>,
        vault: Vault,
    ) -> Result<Self> {
        Self::new_with_secret(config, client, None, store, vault)
    }

    fn new_with_secret(
        config: Arc<Config>,
        client: reqwest::Client,
        client_secret: Option<String>,
        store: Arc<Store>,
        vault: Vault,
    ) -> Result<Self> {
        let tokens = store
            .token()?
            .map(|b| {
                vault
                    .open(&b)
                    .and_then(|p| serde_json::from_slice(&p).map_err(Into::into))
            })
            .transpose()?;
        let token_endpoint = format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            config.graph.tenant_id
        );
        Ok(Self {
            token_endpoint,
            me_endpoint: "https://graph.microsoft.com/v1.0/me?$select=id".into(),
            config,
            client,
            client_secret,
            store,
            vault,
            tokens: Mutex::new(tokens),
            pending: Mutex::new(HashMap::new()),
        })
    }
    pub async fn begin(&self) -> Result<(String, String)> {
        self.begin_with_redirect(self.redirect_uri()).await
    }
    pub async fn begin_desktop(&self, redirect_uri: &str) -> Result<(String, String)> {
        let url = url::Url::parse(redirect_uri)?;
        ensure!(
            url.scheme() == "http"
                && url.host_str() == Some("localhost")
                && url.path() == "/"
                && url.port().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "invalid desktop redirect URI"
        );
        self.begin_with_redirect(redirect_uri.to_owned()).await
    }
    async fn begin_with_redirect(&self, redirect_uri: String) -> Result<(String, String)> {
        let state = URL_SAFE_NO_PAD.encode(random_secret());
        let verifier = URL_SAFE_NO_PAD.encode(random_secret());
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut pending = self.pending.lock().await;
        pending.retain(|_, v| v.expires_at > chrono::Utc::now().timestamp());
        ensure!(pending.len() < 16, "too many authorization attempts");
        pending.insert(
            state.clone(),
            Pending {
                verifier,
                redirect_uri: redirect_uri.clone(),
                expires_at: chrono::Utc::now().timestamp() + 600,
            },
        );
        let mut url = url::Url::parse(&format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/authorize",
            self.config.graph.tenant_id
        ))?;
        url.query_pairs_mut().extend_pairs([
            ("client_id", self.config.graph.client_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", redirect_uri.as_str()),
            ("response_mode", "query"),
            ("scope", &self.config.scopes()),
            ("state", &state),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ]);
        Ok((url.to_string(), state))
    }
    fn redirect_uri(&self) -> String {
        format!(
            "{}/oauth/callback",
            self.config.server.public_url.trim_end_matches('/')
        )
    }
    async fn exchange(&self, mut fields: Vec<(&str, String)>) -> Result<TokenResponse> {
        fields.extend([
            ("client_id", self.config.graph.client_id.clone()),
            ("scope", self.config.scopes()),
        ]);
        if let Some(secret) = &self.client_secret {
            fields.push(("client_secret", secret.clone()));
        }
        let response = self
            .client
            .post(&self.token_endpoint)
            .form(&fields)
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "Entra token exchange failed; interactive authorization may be needed"
        );
        super::bounded_json(response, 64_000).await
    }
    pub async fn complete(&self, state: &str, cookie: &str, code: &str) -> Result<()> {
        ensure!(constant_eq(state, cookie), "OAuth state/cookie mismatch");
        self.complete_pending(state, code).await.map(|_| ())
    }
    pub async fn complete_desktop(&self, state: &str, code: &str) -> Result<String> {
        ensure!(
            self.client_secret.is_none(),
            "desktop login requires public client"
        );
        self.complete_pending(state, code).await
    }
    async fn complete_pending(&self, state: &str, code: &str) -> Result<String> {
        let pending = self
            .pending
            .lock()
            .await
            .remove(state)
            .context("unknown or reused OAuth state")?;
        ensure!(
            pending.expires_at > chrono::Utc::now().timestamp(),
            "expired OAuth state"
        );
        let response = self
            .exchange(vec![
                ("grant_type", "authorization_code".into()),
                ("code", code.into()),
                ("redirect_uri", pending.redirect_uri),
                ("code_verifier", pending.verifier),
            ])
            .await?;
        let me = self
            .client
            .get(&self.me_endpoint)
            .bearer_auth(&response.access_token)
            .send()
            .await?;
        ensure!(me.status().is_success(), "cannot verify authorized user");
        let me: serde_json::Value = super::bounded_json(me, 8000).await?;
        let user_id = me["id"]
            .as_str()
            .context("Graph did not return a user ID")?;
        uuid::Uuid::parse_str(user_id)?;
        if self.config.graph.user_id != uuid::Uuid::nil().to_string() {
            ensure!(
                user_id.eq_ignore_ascii_case(&self.config.graph.user_id),
                "authorized account does not match configured user"
            );
        }
        let tokens = Tokens {
            access_token: response.access_token,
            refresh_token: response
                .refresh_token
                .context("offline_access did not return refresh token")?,
            expires_at: chrono::Utc::now().timestamp() + response.expires_in,
        };
        let mut guard = self.tokens.lock().await;
        self.store
            .put_token(&self.vault.seal(&serde_json::to_vec(&tokens)?)?)?;
        *guard = Some(tokens);
        self.store.event("oauth", "authorized")?;
        Ok(user_id.to_owned())
    }
}
#[async_trait]
impl AccessToken for OAuth {
    async fn access_token(&self) -> Result<String> {
        let mut guard = self.tokens.lock().await;
        let current = guard
            .as_ref()
            .context("interactive Entra authorization required")?;
        if current.expires_at > chrono::Utc::now().timestamp() + 120 {
            return Ok(current.access_token.clone());
        }
        let response = self
            .exchange(vec![
                ("grant_type", "refresh_token".into()),
                ("refresh_token", current.refresh_token.clone()),
            ])
            .await?;
        let tokens = Tokens {
            access_token: response.access_token,
            refresh_token: response
                .refresh_token
                .unwrap_or_else(|| current.refresh_token.clone()),
            expires_at: chrono::Utc::now().timestamp() + response.expires_in,
        };
        self.store
            .put_token(&self.vault.seal(&serde_json::to_vec(&tokens)?)?)?;
        let access = tokens.access_token.clone();
        *guard = Some(tokens);
        Ok(access)
    }
    async fn invalidate(&self) {
        if let Some(t) = self.tokens.lock().await.as_mut() {
            t.expires_at = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, method, path},
    };
    fn config() -> Arc<Config> {
        Arc::new(toml::from_str(include_str!("../../config.example.toml")).unwrap())
    }
    #[tokio::test]
    async fn refresh_is_serialized_rotated_and_encrypted() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("tokens.db")).unwrap());
        let key = STANDARD.encode([5; 32]);
        let vault = Vault::new(&key).unwrap();
        let old = Tokens {
            access_token: "test-old-access".into(),
            refresh_token: "test-old-refresh".into(),
            expires_at: 0,
        };
        store
            .put_token(&vault.seal(&serde_json::to_vec(&old).unwrap()).unwrap())
            .unwrap();
        let mut oauth = OAuth::new(
            config(),
            reqwest::Client::new(),
            "test-client-secret".into(),
            store.clone(),
            vault,
        )
        .unwrap();
        oauth.token_endpoint = format!("{}/token", server.uri());
        Mock::given(method("POST")).and(path("/token")).and(body_string_contains("grant_type=refresh_token")).and(body_string_contains("refresh_token=test-old-refresh")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token":"test-new-access","refresh_token":"test-new-refresh","expires_in":3600}))).expect(1).mount(&server).await;
        let (a, b) = tokio::join!(oauth.access_token(), oauth.access_token());
        assert_eq!(a.unwrap(), "test-new-access");
        assert_eq!(b.unwrap(), "test-new-access");
        let sealed = store.token().unwrap().unwrap();
        assert!(!String::from_utf8_lossy(&sealed).contains("test-new-refresh"));
        let saved: Tokens =
            serde_json::from_slice(&Vault::new(&key).unwrap().open(&sealed).unwrap()).unwrap();
        assert_eq!(saved.refresh_token, "test-new-refresh");
    }
    #[tokio::test]
    async fn authorization_checks_cookie_pkce_identity_and_replay() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("tokens.db")).unwrap());
        let mut oauth = OAuth::new(
            config(),
            reqwest::Client::new(),
            "test-client-secret".into(),
            store.clone(),
            Vault::new(&STANDARD.encode([3; 32])).unwrap(),
        )
        .unwrap();
        oauth.token_endpoint = format!("{}/token", server.uri());
        oauth.me_endpoint = format!("{}/me", server.uri());
        let (url, state) = oauth.begin().await.unwrap();
        assert!(url.contains("code_challenge_method=S256"));
        assert!(
            oauth
                .complete(&state, "wrong-cookie", "test-code")
                .await
                .is_err()
        );
        assert!(server.received_requests().await.unwrap().is_empty());
        Mock::given(method("POST")).and(path("/token")).and(body_string_contains("code_verifier=")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token":"test-access","refresh_token":"test-refresh","expires_in":3600}))).expect(1).mount(&server).await;
        Mock::given(method("GET"))
            .and(path("/me"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"wrong-account"})),
            )
            .mount(&server)
            .await;
        assert!(oauth.complete(&state, &state, "test-code").await.is_err());
        assert!(store.token().unwrap().is_none());
        assert!(oauth.complete(&state, &state, "test-code").await.is_err());
    }

    #[tokio::test]
    async fn desktop_pkce_uses_existing_scopes_without_client_secret() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("tokens.db")).unwrap());
        let mut config = (*config()).clone();
        config.graph.user_id = uuid::Uuid::nil().to_string();
        let mut oauth = OAuth::new_public(
            Arc::new(config),
            reqwest::Client::new(),
            store.clone(),
            Vault::new(&STANDARD.encode([7; 32])).unwrap(),
        )
        .unwrap();
        oauth.token_endpoint = format!("{}/token", server.uri());
        oauth.me_endpoint = format!("{}/me", server.uri());
        assert!(
            oauth
                .begin_desktop("http://evil.example/oauth/callback")
                .await
                .is_err()
        );
        assert!(
            oauth
                .begin_desktop("http://localhost:34567/oauth/callback")
                .await
                .is_err()
        );
        let (url, state) = oauth
            .begin_desktop("http://localhost:34567/")
            .await
            .unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        let query: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(
            query["scope"],
            "offline_access User.Read Chat.Read ChatMessage.Send"
        );
        assert_eq!(query["redirect_uri"], "http://localhost:34567/");
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("code_verifier="))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token":"test-access-token",
                "refresh_token":"test-refresh-token",
                "expires_in":3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        let user_id = uuid::Uuid::new_v4().to_string();
        Mock::given(method("GET"))
            .and(path("/me"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":user_id})),
            )
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            oauth.complete_desktop(&state, "test-code").await.unwrap(),
            user_id
        );
        assert!(store.token().unwrap().is_some());
        let request = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.url.path() == "/token")
            .unwrap();
        assert!(!String::from_utf8_lossy(&request.body).contains("client_secret"));
    }
}
