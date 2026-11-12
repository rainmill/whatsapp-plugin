//! The template mirror: what Meta holds for the account, copied here so the
//! admin can list and preview it and a send can be checked before it is made.

use chrono::{SecondsFormat, Utc};
use laterite_core::query::{bind_values, build};
use laterite_core::sea_query;
use laterite_core::serde_json::{self, Value};
use laterite_core::sqlx;
use laterite_core::{AnyRowExt, Crypt, Db};
use sea_query::{Expr, Order, Query};

use crate::graph::{Method, Request};
use crate::schema::WhatsappTemplates as Templates;
use crate::{Account, WhatsAppError};

/// A message template, as Meta last reported it.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    /// The row id.
    pub id: i64,
    /// Meta's id.
    pub wa_id: String,
    /// The template's name.
    pub name: String,
    /// Its language, such as `en` or `en_US`.
    pub language: String,
    /// MARKETING, UTILITY or AUTHENTICATION.
    pub category: String,
    /// APPROVED, PENDING, REJECTED and the rest of Meta's states.
    pub status: String,
    /// Why Meta rejected it, when it said.
    pub rejected_reason: Option<String>,
    /// The components as Meta returns them.
    pub components: Value,
    /// When it was last read from Meta.
    pub synced_at: String,
}

impl Template {
    /// Whether Meta lets it be sent.
    pub fn approved(&self) -> bool {
        self.status == "APPROVED"
    }

    fn component(&self, kind: &str) -> Option<&Value> {
        self.components.as_array()?.iter().find(|c| {
            c.get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| t.eq_ignore_ascii_case(kind))
        })
    }

    /// How many `{{n}}` parameters the body takes.
    pub fn body_params(&self) -> usize {
        let body = self
            .component("BODY")
            .and_then(|c| c.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        (1..=99)
            .take_while(|n| body.contains(&format!("{{{{{n}}}}}")))
            .count()
    }

    /// Whether its first URL button ends in a parameter.
    pub fn button_param(&self) -> bool {
        self.buttons()
            .iter()
            .any(|(kind, _, target)| kind == "URL" && target.contains("{{1}}"))
    }

    /// The buttons as `(type, text, url or phone number)`.
    fn buttons(&self) -> Vec<(String, String, String)> {
        let text = |b: &Value, key: &str| {
            b.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        self.component("BUTTONS")
            .and_then(|c| c.get("buttons"))
            .and_then(Value::as_array)
            .map(|buttons| {
                buttons
                    .iter()
                    .map(|b| {
                        let target = match text(b, "url") {
                            url if url.is_empty() => text(b, "phone_number"),
                            url => url,
                        };
                        (
                            text(b, "type").to_ascii_uppercase(),
                            text(b, "text"),
                            target,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The message as a recipient reads it, with `body` and `button` filled
    /// in. A parameter with no value keeps Meta's own sample, or its `{{n}}`.
    pub fn preview(&self, body: &[String], button: Option<&str>) -> Preview {
        let text_of = |kind: &str| {
            self.component(kind)
                .and_then(|c| c.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let samples = |kind: &str, pointer: &str| -> Vec<String> {
            self.component(kind)
                .and_then(|c| c.pointer(pointer))
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        let fill = |text: String, given: &[String], samples: &[String]| {
            let mut out = text;
            for n in 1..=given.len().max(samples.len()) {
                if let Some(value) = given.get(n - 1).or_else(|| samples.get(n - 1)) {
                    out = out.replace(&format!("{{{{{n}}}}}"), value);
                }
            }
            out
        };
        let header = self
            .component("HEADER")
            .filter(|c| {
                c.get("format")
                    .and_then(Value::as_str)
                    .is_none_or(|f| f.eq_ignore_ascii_case("TEXT"))
            })
            .map(|_| {
                fill(
                    text_of("HEADER"),
                    &[],
                    &samples("HEADER", "/example/header_text"),
                )
            })
            .filter(|h| !h.is_empty());
        let button_samples: Vec<String> = self
            .component("BUTTONS")
            .and_then(|c| c.get("buttons"))
            .and_then(Value::as_array)
            .and_then(|buttons| {
                buttons.iter().find_map(|b| {
                    b.get("example")
                        .and_then(Value::as_array)
                        .and_then(|e| e.first())
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
            })
            .into_iter()
            .collect();
        let given_button: Vec<String> = button.map(str::to_string).into_iter().collect();
        Preview {
            header,
            body: fill(
                text_of("BODY"),
                body,
                &samples("BODY", "/example/body_text/0"),
            ),
            footer: Some(text_of("FOOTER")).filter(|f| !f.is_empty()),
            buttons: self
                .buttons()
                .into_iter()
                .map(|(kind, text, target)| {
                    (kind, text, fill(target, &given_button, &button_samples))
                })
                .collect(),
        }
    }
}

/// A template as a recipient reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    /// The header line, when the template has a text header.
    pub header: Option<String>,
    /// The body.
    pub body: String,
    /// The footer line.
    pub footer: Option<String>,
    /// The buttons as `(type, text, url or phone number)`.
    pub buttons: Vec<(String, String, String)>,
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn from_row(row: &sqlx::any::AnyRow) -> Result<Template, sqlx::Error> {
    use sqlx::Row;
    Ok(Template {
        id: row.try_get::<i64, _>("id")?,
        wa_id: row.get_text("wa_id")?,
        name: row.get_text("name")?,
        language: row.get_text("language")?,
        category: row.get_text("category")?,
        status: row.get_text("status")?,
        rejected_reason: row
            .get_text_opt("rejected_reason")?
            .filter(|r| !r.is_empty() && r != "NONE"),
        components: serde_json::from_str(&row.get_text("components")?).unwrap_or(Value::Null),
        synced_at: row.get_text("synced_at")?,
    })
}

const COLUMNS: [Templates; 9] = [
    Templates::Id,
    Templates::WaId,
    Templates::Name,
    Templates::Language,
    Templates::Category,
    Templates::Status,
    Templates::RejectedReason,
    Templates::Components,
    Templates::SyncedAt,
];

/// Every mirrored template, by name then language.
pub async fn list(db: &Db) -> Result<Vec<Template>, WhatsAppError> {
    let (sql, values) = build(
        db.backend,
        Query::select()
            .columns(COLUMNS)
            .from(Templates::Table)
            .order_by(Templates::Name, Order::Asc)
            .order_by(Templates::Language, Order::Asc)
            .to_owned(),
    );
    let rows = bind_values(sqlx::query(&sql), values)
        .fetch_all(&db.pool)
        .await?;
    Ok(rows.iter().map(from_row).collect::<Result<_, _>>()?)
}

/// The mirrored template with this row id.
pub async fn find(db: &Db, id: i64) -> Result<Option<Template>, WhatsAppError> {
    let (sql, values) = build(
        db.backend,
        Query::select()
            .columns(COLUMNS)
            .from(Templates::Table)
            .and_where(Expr::col(Templates::Id).eq(id))
            .to_owned(),
    );
    let row = bind_values(sqlx::query(&sql), values)
        .fetch_optional(&db.pool)
        .await?;
    Ok(row.as_ref().map(from_row).transpose()?)
}

/// The mirrored template with this name and language.
pub async fn named(db: &Db, name: &str, language: &str) -> Result<Option<Template>, WhatsAppError> {
    let (sql, values) = build(
        db.backend,
        Query::select()
            .columns(COLUMNS)
            .from(Templates::Table)
            .and_where(Expr::col(Templates::Name).eq(name))
            .and_where(Expr::col(Templates::Language).eq(language))
            .to_owned(),
    );
    let row = bind_values(sqlx::query(&sql), values)
        .fetch_optional(&db.pool)
        .await?;
    Ok(row.as_ref().map(from_row).transpose()?)
}

/// Writes one template Meta reported: a new row, or the existing one for its
/// name and language brought up to date.
pub(crate) async fn store(db: &Db, template: &Value, at: &str) -> Result<(), WhatsAppError> {
    let text = |key: &str| {
        template
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let (name, language) = (text("name"), text("language"));
    if name.is_empty() || language.is_empty() {
        return Ok(());
    }
    let components = template
        .get("components")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()))
        .to_string();
    let existing = named(db, &name, &language).await?;
    let (sql, values) = match existing {
        Some(row) => build(
            db.backend,
            Query::update()
                .table(Templates::Table)
                .value(Templates::WaId, text("id"))
                .value(Templates::Category, text("category").to_ascii_uppercase())
                .value(Templates::Status, text("status").to_ascii_uppercase())
                .value(Templates::RejectedReason, text("rejected_reason"))
                .value(Templates::Components, components)
                .value(Templates::SyncedAt, at)
                .and_where(Expr::col(Templates::Id).eq(row.id))
                .to_owned(),
        ),
        None => build(
            db.backend,
            Query::insert()
                .into_table(Templates::Table)
                .columns([
                    Templates::WaId,
                    Templates::Name,
                    Templates::Language,
                    Templates::Category,
                    Templates::Status,
                    Templates::RejectedReason,
                    Templates::Components,
                    Templates::SyncedAt,
                ])
                .values_panic([
                    text("id").into(),
                    name.into(),
                    language.into(),
                    text("category").to_ascii_uppercase().into(),
                    text("status").to_ascii_uppercase().into(),
                    text("rejected_reason").into(),
                    components.into(),
                    at.into(),
                ])
                .to_owned(),
        ),
    };
    bind_values(sqlx::query(&sql), values)
        .execute(&db.pool)
        .await?;
    Ok(())
}

/// Sets the status Meta reported for a template, by name and language. Meta
/// writes the language with a hyphen here and an underscore elsewhere.
pub(crate) async fn set_status(
    db: &Db,
    name: &str,
    language: &str,
    status: &str,
    reason: Option<&str>,
) -> Result<(), WhatsAppError> {
    let (sql, values) = build(
        db.backend,
        Query::update()
            .table(Templates::Table)
            .value(Templates::Status, status.to_ascii_uppercase())
            .value(Templates::RejectedReason, reason.unwrap_or_default())
            .value(Templates::SyncedAt, now())
            .and_where(Expr::col(Templates::Name).eq(name))
            .and_where(
                Expr::col(Templates::Language)
                    .is_in([language.replace('-', "_"), language.to_string()]),
            )
            .to_owned(),
    );
    bind_values(sqlx::query(&sql), values)
        .execute(&db.pool)
        .await?;
    Ok(())
}

/// Reads every template from Meta into the mirror and drops the rows Meta no
/// longer has. Returns how many it holds.
pub async fn sync(db: &Db, crypt: &Crypt) -> Result<usize, WhatsAppError> {
    let account = Account::lease(db, crypt).await?;
    let at = now();
    let mut path = format!(
        "{}/message_templates?fields=id,name,language,status,category,components,rejected_reason&limit=100",
        account.business_account_id
    );
    let mut count = 0;
    // A page at a time, as far as Meta says there is more.
    for _ in 0..50 {
        let answer = match account.call(Method::Get, &path, None).await {
            Ok(answer) => answer,
            Err(e) => {
                account.failed(&e).await;
                return Err(e.into());
            }
        };
        for template in answer
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            store(db, template, &at).await?;
            count += 1;
        }
        let after = answer
            .pointer("/paging/cursors/after")
            .and_then(Value::as_str);
        match (answer.pointer("/paging/next"), after) {
            (Some(next), Some(after)) if !next.is_null() => {
                path = format!(
                    "{}/message_templates?fields=id,name,language,status,category,components,rejected_reason&limit=100&after={after}",
                    account.business_account_id
                );
            }
            _ => break,
        }
    }
    let (sql, values) = build(
        db.backend,
        Query::delete()
            .from_table(Templates::Table)
            .and_where(Expr::col(Templates::SyncedAt).ne(at.as_str()))
            .to_owned(),
    );
    bind_values(sqlx::query(&sql), values)
        .execute(&db.pool)
        .await?;
    account.used().await;
    Ok(count)
}

/// Sends a request built by the account: used by [`sync`] and the screens.
impl Account {
    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, crate::graph::GraphError> {
        crate::graph::graph()
            .call(Request {
                method,
                version: self.version.clone(),
                path: path.to_string(),
                token: self.token.clone(),
                body,
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template() -> Template {
        Template {
            id: 1,
            wa_id: "9".into(),
            name: "order_ready".into(),
            language: "en".into(),
            category: "UTILITY".into(),
            status: "APPROVED".into(),
            rejected_reason: None,
            components: serde_json::json!([
                { "type": "HEADER", "format": "TEXT", "text": "Order {{1}}", "example": { "header_text": ["A-100"] } },
                { "type": "BODY", "text": "Hi {{1}}, your order is ready at {{2}}.", "example": { "body_text": [["Asha", "Indiranagar"]] } },
                { "type": "FOOTER", "text": "Acme" },
                { "type": "BUTTONS", "buttons": [
                    { "type": "URL", "text": "Track", "url": "https://acme.test/o/{{1}}", "example": ["A-100"] },
                    { "type": "QUICK_REPLY", "text": "Thanks" }
                ] }
            ]),
            synced_at: String::new(),
        }
    }

    #[test]
    fn a_preview_fills_given_values_then_metas_samples() {
        let t = template();
        assert_eq!(t.body_params(), 2);
        assert!(t.button_param());
        let sample = t.preview(&[], None);
        assert_eq!(sample.header.as_deref(), Some("Order A-100"));
        assert_eq!(sample.body, "Hi Asha, your order is ready at Indiranagar.");
        assert_eq!(sample.footer.as_deref(), Some("Acme"));
        assert_eq!(sample.buttons[0].2, "https://acme.test/o/A-100");
        let filled = t.preview(&["Ravi".to_string()], Some("B-7"));
        assert_eq!(filled.body, "Hi Ravi, your order is ready at Indiranagar.");
        assert_eq!(filled.buttons[0].2, "https://acme.test/o/B-7");
        assert_eq!(
            filled.buttons[1],
            ("QUICK_REPLY".into(), "Thanks".into(), String::new())
        );
    }
}
