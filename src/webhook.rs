//! Meta's deliveries: what became of a message, and a template's review.
//!
//! Meta proves a delivery is its own with a signature over the raw body, keyed
//! by the app secret stored on the credential. An unsigned delivery is refused.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, SecondsFormat, Utc};
use hmac::{Hmac, Mac};
use laterite_admin::axum::body::Bytes;
use laterite_admin::axum::extract::Query as UrlQuery;
use laterite_admin::axum::http::{HeaderMap, StatusCode};
use laterite_admin::axum::response::{IntoResponse, Response};
use laterite_admin::axum::routing::get;
use laterite_admin::axum::Router;
use laterite_admin::routes::{PublicRoute, PublicRouteReg, RouteCtx};
use laterite_admin::AdminRegistry;
use laterite_core::query::{bind_values, build};
use laterite_core::sea_query;
use laterite_core::serde_json::{self, Value};
use laterite_core::sqlx;
use laterite_core::{AnyRowExt, Db, Registry};
use sea_query::{Expr, Query};
use sha2::Sha256;

use crate::schema::WhatsappMessages as Messages;
use crate::{templates, SLOT};

/// Where Meta is told to deliver.
pub const PATH: &str = "/webhooks/whatsapp";

pub(crate) fn register(registry: &mut Registry) {
    registry.add_public_route(PublicRouteReg::new(PATH, Arc::new(Webhook)).skip_origin_check());
}

struct Webhook;

impl PublicRoute for Webhook {
    fn mount(&self, ctx: &RouteCtx) -> Router {
        let (verify_cx, receive_cx) = (ctx.clone(), ctx.clone());
        Router::new().route(
            "/",
            get(move |UrlQuery(query): UrlQuery<HashMap<String, String>>| {
                let ctx = verify_cx.clone();
                async move { verify(&ctx, &query).await }
            })
            .post(move |headers: HeaderMap, body: Bytes| {
                let ctx = receive_cx.clone();
                async move { receive(&ctx, &headers, &body).await }
            }),
        )
    }
}

/// One field of the stored credential. Nothing is spent reading it.
async fn secret(ctx: &RouteCtx, field: &str) -> Option<String> {
    let lease = rainmill_vault::lease(ctx.db(), ctx.crypt(), SLOT)
        .await
        .ok()?;
    lease
        .field(field)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Meta's check that the address is ours: it sends a token we chose and a
/// challenge to answer with.
async fn verify(ctx: &RouteCtx, query: &HashMap<String, String>) -> Response {
    let get = |key: &str| query.get(key).map(String::as_str).unwrap_or_default();
    let expected = secret(ctx, "verify_token").await;
    let matches = expected
        .as_deref()
        .is_some_and(|token| same(token.as_bytes(), get("hub.verify_token").as_bytes()));
    if get("hub.mode") == "subscribe" && matches {
        (StatusCode::OK, get("hub.challenge").to_string()).into_response()
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}

/// Compares without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Whether `header` (`sha256=<hex>`) is the HMAC-SHA256 of `body` under `key`.
pub(crate) fn signed(key: &[u8], body: &[u8], header: &str) -> bool {
    let Some(hex) = header.strip_prefix("sha256=") else {
        return false;
    };
    let bytes: Option<Vec<u8>> = (0..hex.len())
        .step_by(2)
        .map(|i| {
            hex.get(i..i + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
        })
        .collect();
    let (Some(bytes), Ok(mut mac)) = (bytes, Hmac::<Sha256>::new_from_slice(key)) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&bytes).is_ok()
}

async fn receive(ctx: &RouteCtx, headers: &HeaderMap, body: &[u8]) -> Response {
    let Some(key) = secret(ctx, "app_secret").await else {
        tracing::warn!("a WhatsApp delivery arrived and no app secret is stored to check it");
        return StatusCode::FORBIDDEN.into_response();
    };
    let header = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !signed(key.as_bytes(), body, header) {
        tracing::warn!("a WhatsApp delivery with a bad signature was refused");
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(payload) = serde_json::from_slice::<Value>(body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match apply(ctx.db(), &payload).await {
        Ok(_) => StatusCode::OK.into_response(),
        // Meta retries anything but a 200, which is what a database that is
        // briefly away needs.
        Err(e) => {
            tracing::error!(error = %e, "a WhatsApp delivery could not be applied");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

/// A Unix time, as Meta sends it (a string), in the form the tables keep.
fn stamp(value: Option<&Value>) -> String {
    value
        .and_then(|v| {
            v.as_str()
                .map(str::to_string)
                .or(v.as_i64().map(|n| n.to_string()))
        })
        .and_then(|s| s.parse::<i64>().ok())
        .and_then(|secs| DateTime::<Utc>::from_timestamp(secs, 0))
        .unwrap_or_else(Utc::now)
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// How far along a state is, so a late delivery never moves a message back.
fn rank(state: &str) -> u8 {
    match state {
        "queued" => 0,
        "sent" => 1,
        "delivered" => 2,
        "read" => 3,
        _ => 0,
    }
}

/// Applies a delivery's changes and returns how many it acted on.
pub async fn apply(db: &Db, payload: &Value) -> Result<usize, sqlx::Error> {
    let mut applied = 0;
    // Gathered first: nothing lazy is held while the database is awaited.
    let mut changes: Vec<&Value> = Vec::new();
    if let Some(entries) = payload.get("entry").and_then(Value::as_array) {
        for entry in entries {
            if let Some(list) = entry.get("changes").and_then(Value::as_array) {
                changes.extend(list);
            }
        }
    }
    for change in changes {
        let value = change.get("value").unwrap_or(&Value::Null);
        match change.get("field").and_then(Value::as_str) {
            Some("messages") => {
                let statuses: Vec<&Value> = match value.get("statuses").and_then(Value::as_array) {
                    Some(list) => list.iter().collect(),
                    None => Vec::new(),
                };
                for status in statuses {
                    if status_update(db, status).await? {
                        applied += 1;
                    }
                }
            }
            Some("message_template_status_update") => {
                let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default();
                let reason = Some(text("reason")).filter(|r| !r.is_empty() && *r != "NONE");
                if let Err(e) = templates::set_status(
                    db,
                    text("message_template_name"),
                    text("message_template_language"),
                    text("event"),
                    reason,
                )
                .await
                {
                    tracing::error!(error = %e, "a template's status could not be stored");
                } else {
                    applied += 1;
                }
            }
            _ => {}
        }
    }
    Ok(applied)
}

/// Moves one message to the state a status reports. `false` for a message
/// this application did not send, and for a report older than what is stored.
async fn status_update(db: &Db, status: &Value) -> Result<bool, sqlx::Error> {
    let text = |key: &str| status.get(key).and_then(Value::as_str).unwrap_or_default();
    let (wa_id, state) = (text("id"), text("status"));
    if wa_id.is_empty() {
        return Ok(false);
    }
    let (sql, values) = build(
        db.backend,
        Query::select()
            .columns([Messages::Id, Messages::State])
            .from(Messages::Table)
            .and_where(Expr::col(Messages::WaMessageId).eq(wa_id))
            .to_owned(),
    );
    let Some(row) = bind_values(sqlx::query(&sql), values)
        .fetch_optional(&db.pool)
        .await?
    else {
        return Ok(false);
    };
    let id = sqlx::Row::try_get::<i64, _>(&row, "id")?;
    let current = row.get_text("state")?;
    let at = stamp(status.get("timestamp"));

    // Built and bound in one block: the statement must not outlive it.
    let (sql, values) = {
        let mut update = Query::update();
        update
            .table(Messages::Table)
            .and_where(Expr::col(Messages::Id).eq(id));
        let mut changed = false;
        // The charge rides on the sent report or the delivered one, never both.
        if let Some(pricing) = status.get("pricing") {
            let kind = pricing.get("type").and_then(Value::as_str);
            let billable = match kind {
                Some(kind) => kind == "regular",
                None => pricing
                    .get("billable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };
            update.value(Messages::Billable, i32::from(billable));
            if let Some(category) = pricing.get("category").and_then(Value::as_str) {
                update.value(Messages::PricingCategory, category);
            }
            changed = true;
        }
        match state {
            "sent" | "delivered" | "read"
                if rank(state) > rank(&current) && current != "failed" =>
            {
                update.value(Messages::State, state);
                update.value(
                    match state {
                        "sent" => Messages::SentAt,
                        "delivered" => Messages::DeliveredAt,
                        _ => Messages::ReadAt,
                    },
                    at,
                );
                changed = true;
            }
            // Meta gives up on a message it had accepted.
            "failed" if rank(&current) < rank("delivered") => {
                let error = status.pointer("/errors/0").unwrap_or(&Value::Null);
                let words = error
                    .pointer("/error_data/details")
                    .or(error.get("message"))
                    .or(error.get("title"))
                    .and_then(Value::as_str)
                    .unwrap_or("Meta could not deliver the message.");
                update
                    .value(Messages::State, "failed")
                    .value(
                        Messages::ErrorCode,
                        error
                            .get("code")
                            .and_then(Value::as_i64)
                            .map(|c| c.to_string()),
                    )
                    .value(Messages::ErrorText, words)
                    .value(Messages::FailedAt, at);
                changed = true;
            }
            _ => {}
        }
        if !changed {
            return Ok(false);
        }
        build(db.backend, update)
    };
    bind_values(sqlx::query(&sql), values)
        .execute(&db.pool)
        .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delivery_is_ours_only_with_the_right_signature() {
        // HMAC-SHA256("key", "The quick brown fox jumps over the lazy dog").
        let body = b"The quick brown fox jumps over the lazy dog";
        let good = "sha256=f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8";
        assert!(signed(b"key", body, good));
        assert!(!signed(b"other", body, good));
        assert!(!signed(b"key", b"tampered", good));
        assert!(!signed(b"key", body, "f7bc83"));
        assert!(!signed(b"key", body, "sha256=zz"));
    }

    #[test]
    fn a_unix_time_becomes_a_stored_timestamp() {
        assert_eq!(
            stamp(Some(&Value::from("1750263773"))),
            "2025-06-18T16:22:53Z"
        );
    }
}
