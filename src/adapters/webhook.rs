use crate::{
    adapters::{graph::Graph, teams},
    security::constant_eq,
};
use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use std::{collections::HashMap, sync::Arc};

pub struct WebState {
    pub graph: Arc<Graph>,
}
/// Public listener reached through the tunnel: only Graph callbacks and liveness.
/// Administration lives on the desktop host's authenticated loopback channel.
pub fn router(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/graph/notifications", post(notifications))
        .route("/graph/lifecycle", post(lifecycle))
        .layer(DefaultBodyLimit::max(256_000))
        .with_state(state)
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
            let collection = teams::collection(&resource).unwrap();
            if collection != sub.resource
                && !(sub.resource == state.graph.user_messages_resource()
                    && collection.starts_with("chats/"))
            {
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
