use crate::{
    adapters::{graph::Graph, oauth::OAuth, teams},
    security::constant_eq,
};
use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Form, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;
use std::{collections::HashMap, sync::Arc};

pub struct WebState {
    pub graph: Arc<Graph>,
    pub oauth: Arc<OAuth>,
    pub admin_key: String,
}
pub fn router(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/oauth/login", get(login))
        .route("/oauth/start", post(start))
        .route("/oauth/callback", get(callback))
        .route("/graph/notifications", post(notifications))
        .route("/graph/lifecycle", post(lifecycle))
        .layer(DefaultBodyLimit::max(256_000))
        .with_state(state)
}
async fn login() -> impl IntoResponse {
    (
        [
            (header::CACHE_CONTROL, "no-store"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; form-action 'self' https://login.microsoftonline.com; frame-ancestors 'none'",
            ),
        ],
        Html(
            "<!doctype html><html lang=es><meta charset=utf-8><title>Conectar Teams</title><h1>Conectar mi cuenta de Teams</h1><form action=/oauth/start method=post><label>Clave de administración <input name=key type=password required autocomplete=current-password></label><button>Continuar con Microsoft</button></form></html>",
        ),
    )
}
#[derive(Deserialize)]
struct Login {
    key: String,
}
async fn start(State(state): State<Arc<WebState>>, Form(form): Form<Login>) -> Response {
    if !constant_eq(&form.key, &state.admin_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.oauth.begin().await {
        Ok((url, csrf)) => {
            let mut r = Redirect::to(&url).into_response();
            r.headers_mut().insert(
                header::SET_COOKIE,
                format!(
                    "teams_oauth={csrf}; HttpOnly; Secure; SameSite=Lax; Path=/oauth; Max-Age=600"
                )
                .parse()
                .unwrap(),
            );
            r.headers_mut()
                .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
            r
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
async fn callback(
    State(state): State<Arc<WebState>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .find_map(|p| p.trim().strip_prefix("teams_oauth="))
        })
        .unwrap_or("");
    let (Some(csrf), Some(code)) = (query.get("state"), query.get("code")) else {
        return (
            StatusCode::BAD_REQUEST,
            "Autorización incompleta; vuelve a iniciar sesión.",
        )
            .into_response();
    };
    match state.oauth.complete(csrf, cookie, code).await {
        Ok(()) => (
            [
                (
                    header::SET_COOKIE,
                    "teams_oauth=; HttpOnly; Secure; SameSite=Lax; Path=/oauth; Max-Age=0",
                ),
                (header::CACHE_CONTROL, "no-store"),
                (header::REFERRER_POLICY, "no-referrer"),
            ],
            "Cuenta conectada. Puedes cerrar esta ventana.",
        )
            .into_response(),
        Err(_) => {
            tracing::warn!(event = "oauth_failed");
            (
                StatusCode::BAD_REQUEST,
                "No se pudo autorizar la cuenta configurada.",
            )
                .into_response()
        }
    }
}
#[derive(Deserialize)]
struct Batch {
    value: Vec<Notification>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Notification {
    subscription_id: String,
    client_state: Option<String>,
    tenant_id: Option<String>,
    resource: Option<String>,
    change_type: Option<String>,
    lifecycle_event: Option<String>,
}
async fn notifications(
    State(state): State<Arc<WebState>>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    receive(state, query, body, false).await
}
async fn lifecycle(
    State(state): State<Arc<WebState>>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    receive(state, query, body, true).await
}
async fn receive(
    state: Arc<WebState>,
    query: HashMap<String, String>,
    body: Bytes,
    lifecycle: bool,
) -> Response {
    if let Some(token) = query.get("validationToken") {
        if token.len() > 2048 {
            return StatusCode::BAD_REQUEST.into_response();
        }
        return (
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            token.clone(),
        )
            .into_response();
    }
    let batch: Batch = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if batch.value.len() > 100 {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let subscriptions = match state.graph.store.subscriptions() {
        Ok(s) => s,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    // Authenticate the entire batch before persisting any work; no provider calls on webhook thread.
    for n in &batch.value {
        let sub = subscriptions.iter().find(|s| s.id == n.subscription_id);
        if !n
            .client_state
            .as_ref()
            .is_some_and(|s| constant_eq(s, &state.graph.client_state))
            || n.tenant_id.as_deref() != Some(&state.graph.config.graph.tenant_id)
            || sub.is_none()
        {
            return StatusCode::FORBIDDEN.into_response();
        }
        let sub = sub.unwrap();
        if !state.graph.allowed_collection(&sub.resource) {
            return StatusCode::FORBIDDEN.into_response();
        }
        if !lifecycle {
            let resource = match n.resource.as_deref().map(teams::canonical_resource) {
                Some(Ok(s)) => s,
                _ => return StatusCode::BAD_REQUEST.into_response(),
            };
            if teams::collection(&resource).ok().as_deref() != Some(&sub.resource) {
                return StatusCode::FORBIDDEN.into_response();
            }
        }
    }
    for n in batch.value {
        let result = if lifecycle {
            match n.lifecycle_event.as_deref() {
                Some("subscriptionRemoved") => {
                    state.graph.store.remove_subscription(&n.subscription_id)
                }
                Some("reauthorizationRequired") => {
                    state.graph.store.expire_subscription(&n.subscription_id)
                }
                Some("missed") => state
                    .graph
                    .store
                    .enqueue(&format!(
                        "recovery:{}:{}",
                        n.subscription_id,
                        chrono::Utc::now().timestamp() / 60
                    ))
                    .map(|_| ()),
                _ => Ok(()),
            }
        } else if n.change_type.as_deref() == Some("created") {
            state
                .graph
                .store
                .enqueue(&teams::canonical_resource(n.resource.as_deref().unwrap()).unwrap())
                .map(|_| ())
        } else {
            Ok(())
        };
        if result.is_err() {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    StatusCode::ACCEPTED.into_response()
}
