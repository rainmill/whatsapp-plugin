//! Sending a template message and keeping its row true: queued, then sent,
//! then whatever Meta reports.

use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use laterite_core::events::Event;
use laterite_core::query::{bind_values, build, insert_returning_id};
use laterite_core::queue::{Job, JobCx, JobError, Queue};
use laterite_core::sea_query;
use laterite_core::serde_json::{self, json, Value};
use laterite_core::sqlx;
use laterite_core::strata::async_trait;
use laterite_core::{AnyRowExt, Db};
use sea_query::{Expr, Query};
use serde::{Deserialize, Serialize};

use crate::graph::Method;
use crate::schema::WhatsappMessages as Messages;
use crate::{templates, Account, WhatsAppError};

/// A template message to send.
#[derive(Clone, Debug)]
pub struct Message {
    to: String,
    template: String,
    language: String,
    params: Vec<String>,
    button: Option<String>,
    subject: Option<(String, i64)>,
}

impl Message {
    /// A message from the template `name` in `language`, as the Templates
    /// screen lists them.
    pub fn template(name: impl Into<String>, language: impl Into<String>) -> Self {
        Self {
            to: String::new(),
            template: name.into(),
            language: language.into(),
            params: Vec::new(),
            button: None,
            subject: None,
        }
    }

    /// The recipient's number with its country code: `+919845012345`.
    pub fn to(mut self, number: impl Into<String>) -> Self {
        self.to = number.into();
        self
    }

    /// The next body value: the first call fills `{{1}}`, the second `{{2}}`.
    pub fn param(mut self, value: impl Into<String>) -> Self {
        self.params.push(value.into());
        self
    }

    /// The value the template's URL button ends in.
    pub fn button(mut self, value: impl Into<String>) -> Self {
        self.button = Some(value.into());
        self
    }

    /// The application record the message is about, shown on the log.
    pub fn about(mut self, entity: impl Into<String>, id: i64) -> Self {
        self.subject = Some((entity.into(), id));
        self
    }
}

/// A message was given up on. `id` is its row in the log.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageFailed {
    /// The message's row id, as [`send`] returned it.
    pub id: i64,
    /// Why, in Meta's words when it gave any.
    pub reason: String,
}

impl Event for MessageFailed {
    const NAME: &'static str = "rainmill.whatsapp.message.failed";
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// A number as Meta takes it: digits after one leading `+`. `None` when what
/// is left is no phone number.
pub(crate) fn number(raw: &str) -> Option<String> {
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    ((8..=15).contains(&digits.len()) && raw.trim_start().starts_with('+'))
        .then(|| format!("+{digits}"))
}

/// Queues a message and returns its row id in the log.
///
/// Refused, with nothing queued, when the number has no country code, the
/// template is not in the mirror or not approved, or the values do not match
/// what the template takes.
pub async fn send(queue: &Queue, db: &Db, message: Message) -> Result<i64, WhatsAppError> {
    let refuse = |why: String| Err(WhatsAppError::Refused(why));
    let Some(to) = number(&message.to) else {
        return refuse(format!(
            "`{}` is not a phone number with its country code",
            message.to
        ));
    };
    let Some(template) = templates::named(db, &message.template, &message.language).await? else {
        return refuse(format!(
            "no template `{}` in `{}`: sync the templates first",
            message.template, message.language
        ));
    };
    if !template.approved() {
        return refuse(format!(
            "the template `{}` is {}, not approved",
            template.name,
            template.status.to_lowercase()
        ));
    }
    if template.body_params() != message.params.len() {
        return refuse(format!(
            "the template `{}` takes {} values, {} given",
            template.name,
            template.body_params(),
            message.params.len()
        ));
    }
    if template.button_param() != message.button.is_some() {
        return refuse(format!(
            "the template `{}` {} a button value",
            template.name,
            if template.button_param() {
                "needs"
            } else {
                "takes no"
            }
        ));
    }
    let (subject_type, subject_id) = match &message.subject {
        Some((entity, id)) => (Some(entity.clone()), Some(*id)),
        None => (None, None),
    };
    let insert = Query::insert()
        .into_table(Messages::Table)
        .columns([
            Messages::ToNumber,
            Messages::Template,
            Messages::Language,
            Messages::Category,
            Messages::Params,
            Messages::ButtonParam,
            Messages::State,
            Messages::SubjectType,
            Messages::SubjectId,
            Messages::CreatedAt,
        ])
        .values_panic([
            to.into(),
            template.name.clone().into(),
            template.language.clone().into(),
            template.category.to_lowercase().into(),
            Value::from(message.params.clone()).to_string().into(),
            message.button.clone().into(),
            "queued".into(),
            subject_type.into(),
            subject_id.into(),
            now().into(),
        ])
        .to_owned();
    let id = insert_returning_id(db, insert, Messages::Id).await?;
    if let Err(e) = queue.push(&SendMessage { id }).await {
        fail(
            db,
            id,
            None,
            &format!("The message could not be queued: {e}"),
        )
        .await?;
        return refuse(format!("the message could not be queued: {e}"));
    }
    Ok(id)
}

/// Marks a waiting message failed, with Meta's code and words.
pub(crate) async fn fail(
    db: &Db,
    id: i64,
    code: Option<i64>,
    text: &str,
) -> Result<(), sqlx::Error> {
    let (sql, values) = build(
        db.backend,
        Query::update()
            .table(Messages::Table)
            .value(Messages::State, "failed")
            .value(Messages::ErrorCode, code.map(|c| c.to_string()))
            .value(Messages::ErrorText, text)
            .value(Messages::FailedAt, now())
            .and_where(Expr::col(Messages::Id).eq(id))
            // Only a message still waiting: one already failed keeps its reason.
            .and_where(Expr::col(Messages::State).eq("queued"))
            .to_owned(),
    );
    bind_values(sqlx::query(&sql), values)
        .execute(&db.pool)
        .await?;
    Ok(())
}

/// The request body for one template message.
pub(crate) fn body(
    to: &str,
    template: &str,
    language: &str,
    params: &[String],
    button: Option<&str>,
) -> Value {
    let mut components = Vec::new();
    if !params.is_empty() {
        components.push(json!({
            "type": "body",
            "parameters": params.iter().map(|p| json!({ "type": "text", "text": p })).collect::<Vec<_>>(),
        }));
    }
    if let Some(button) = button {
        components.push(json!({
            "type": "button",
            "sub_type": "url",
            "index": "0",
            "parameters": [{ "type": "text", "text": button }],
        }));
    }
    json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": to,
        "type": "template",
        "template": {
            "name": template,
            "language": { "code": language },
            "components": components,
        },
    })
}

/// A message in the log.
#[derive(Clone, Debug, PartialEq)]
pub struct Sent {
    /// The row id.
    pub id: i64,
    /// The recipient's number.
    pub to: String,
    /// The template's name.
    pub template: String,
    /// The template's category, lowercase.
    pub category: String,
    /// queued, sent, delivered, read or failed.
    pub state: String,
    /// Why it failed, in Meta's words.
    pub error: Option<String>,
    /// Whether Meta charged for it, once it has said.
    pub billable: Option<bool>,
    /// When it was queued.
    pub created_at: String,
}

/// The messages queued from `from` to `to` (both `YYYY-MM-DD`, both
/// included), newest first, at most `limit`, in one state when `state` is set.
pub async fn log(
    db: &Db,
    from: &str,
    to: &str,
    state: Option<&str>,
    limit: u64,
) -> Result<Vec<Sent>, WhatsAppError> {
    let (sql, values) = {
        let mut select = Query::select();
        select
            .columns([
                Messages::Id,
                Messages::ToNumber,
                Messages::Template,
                Messages::Category,
                Messages::State,
                Messages::ErrorText,
                Messages::Billable,
                Messages::CreatedAt,
            ])
            .from(Messages::Table)
            .and_where(Expr::col(Messages::CreatedAt).gte(from))
            // A timestamp sorts after its own date, and before the next day's.
            .and_where(Expr::col(Messages::CreatedAt).lt(format!("{to}T99")))
            .order_by(Messages::Id, sea_query::Order::Desc)
            .limit(limit);
        if let Some(state) = state {
            select.and_where(Expr::col(Messages::State).eq(state));
        }
        build(db.backend, select)
    };
    let rows = bind_values(sqlx::query(&sql), values)
        .fetch_all(&db.pool)
        .await?;
    rows.iter().map(sent_from).collect()
}

/// What became of the messages sent with one template.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// Messages Meta accepted, whatever became of them after.
    pub sent: i64,
    /// Messages that reached the phone.
    pub delivered: i64,
    /// Messages the recipient opened.
    pub read: i64,
    /// Messages that failed.
    pub failed: i64,
    /// Messages still waiting to be sent.
    pub waiting: i64,
    /// Messages Meta charged for.
    pub charged: i64,
}

/// The tally of each template's messages queued since `from` (`YYYY-MM-DD`),
/// by template name and language.
pub async fn tallies(
    db: &Db,
    from: &str,
) -> Result<std::collections::HashMap<(String, String), Tally>, WhatsAppError> {
    let count = sea_query::Alias::new("n");
    let (sql, values) = build(
        db.backend,
        Query::select()
            .columns([
                Messages::Template,
                Messages::Language,
                Messages::State,
                Messages::Billable,
            ])
            .expr_as(Expr::col(Messages::Id).count(), count)
            .from(Messages::Table)
            .and_where(Expr::col(Messages::CreatedAt).gte(from))
            .group_by_columns([
                Messages::Template,
                Messages::Language,
                Messages::State,
                Messages::Billable,
            ])
            .to_owned(),
    );
    let rows = bind_values(sqlx::query(&sql), values)
        .fetch_all(&db.pool)
        .await?;
    let mut out: std::collections::HashMap<(String, String), Tally> = Default::default();
    for row in &rows {
        let n = row.get_int("n")?;
        let tally = out
            .entry((row.get_text("template")?, row.get_text("language")?))
            .or_default();
        match row.get_text("state")?.as_str() {
            "queued" => tally.waiting += n,
            "failed" => tally.failed += n,
            "read" => {
                tally.sent += n;
                tally.delivered += n;
                tally.read += n;
            }
            "delivered" => {
                tally.sent += n;
                tally.delivered += n;
            }
            _ => tally.sent += n,
        }
        let billable = sqlx::Row::try_get::<Option<i32>, _>(row, "billable")
            .ok()
            .flatten();
        if billable.is_some_and(|b| b != 0) {
            tally.charged += n;
        }
    }
    Ok(out)
}

/// The newest messages sent with one template, at most `limit`.
pub async fn recent(
    db: &Db,
    template: &str,
    language: &str,
    limit: u64,
) -> Result<Vec<Sent>, WhatsAppError> {
    let (sql, values) = build(
        db.backend,
        Query::select()
            .columns([
                Messages::Id,
                Messages::ToNumber,
                Messages::Template,
                Messages::Category,
                Messages::State,
                Messages::ErrorText,
                Messages::Billable,
                Messages::CreatedAt,
            ])
            .from(Messages::Table)
            .and_where(Expr::col(Messages::Template).eq(template))
            .and_where(Expr::col(Messages::Language).eq(language))
            .order_by(Messages::Id, sea_query::Order::Desc)
            .limit(limit)
            .to_owned(),
    );
    let rows = bind_values(sqlx::query(&sql), values)
        .fetch_all(&db.pool)
        .await?;
    rows.iter().map(sent_from).collect()
}

fn sent_from(row: &sqlx::any::AnyRow) -> Result<Sent, WhatsAppError> {
    Ok(Sent {
        id: sqlx::Row::try_get::<i64, _>(row, "id")?,
        to: row.get_text("to_number")?,
        template: row.get_text("template")?,
        category: row.get_text("category")?,
        state: row.get_text("state")?,
        error: row.get_text_opt("error_text")?.filter(|e| !e.is_empty()),
        billable: sqlx::Row::try_get::<Option<i32>, _>(row, "billable")
            .ok()
            .flatten()
            .map(|b| b != 0),
        created_at: row.get_text("created_at")?,
    })
}

/// Sends one queued message.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct SendMessage {
    id: i64,
}

/// How many times a message is tried before it is given up on.
const TRIES: u32 = 5;

#[async_trait]
impl Job for SendMessage {
    const NAME: &'static str = "rainmill.whatsapp::send_message";

    async fn run(&self, cx: &JobCx) -> Result<(), JobError> {
        let db = cx.db();
        let (sql, values) = build(
            db.backend,
            Query::select()
                .columns([
                    Messages::ToNumber,
                    Messages::Template,
                    Messages::Language,
                    Messages::Category,
                    Messages::Params,
                    Messages::ButtonParam,
                    Messages::State,
                ])
                .from(Messages::Table)
                .and_where(Expr::col(Messages::Id).eq(self.id))
                .to_owned(),
        );
        let row = bind_values(sqlx::query(&sql), values)
            .fetch_optional(&db.pool)
            .await
            .map_err(JobError::retry)?;
        // Deleted, or already sent by an earlier attempt that lost its answer.
        let Some(row) = row else { return Ok(()) };
        let text = |name: &str| row.get_text(name).unwrap_or_default();
        if text("state") != "queued" {
            return Ok(());
        }
        let params: Vec<String> = serde_json::from_str(&text("params")).unwrap_or_default();
        let button = row.get_text_opt("button_param").ok().flatten();
        let last = cx.attempt() >= TRIES;
        let give_up = |reason: String| async move {
            if let Err(e) = fail(db, self.id, None, &reason).await {
                tracing::error!(error = %e, "the failed message could not be marked");
            }
            cx.events()
                .emit(&MessageFailed {
                    id: self.id,
                    reason: reason.clone(),
                })
                .await;
            JobError::fail(reason)
        };

        let Some(crypt) = cx.crypt() else {
            return Err(give_up("The application key is not available to jobs.".to_string()).await);
        };
        let account = match Account::lease(db, crypt).await {
            Ok(account) => account,
            // Nothing attached, or the token is refused: someone has to act,
            // so the message waits for the next try.
            Err(e) if !last => return Err(JobError::retry(e)),
            Err(e) => return Err(give_up(e.to_string()).await),
        };
        let path = format!("{}/messages", account.phone_number_id);
        let request = body(
            &text("to_number"),
            &text("template"),
            &text("language"),
            &params,
            button.as_deref(),
        );
        match account.call(Method::Post, &path, Some(request)).await {
            Ok(answer) => {
                let wa_id = answer
                    .pointer("/messages/0/id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let (sql, values) = build(
                    db.backend,
                    Query::update()
                        .table(Messages::Table)
                        .value(Messages::State, "sent")
                        .value(Messages::WaMessageId, wa_id)
                        .value(Messages::SentAt, now())
                        .value(Messages::ErrorText, Option::<String>::None)
                        .and_where(Expr::col(Messages::Id).eq(self.id))
                        .to_owned(),
                );
                // Meta has the message: a row that cannot be written must not
                // send it a second time.
                if let Err(e) = bind_values(sqlx::query(&sql), values)
                    .execute(&db.pool)
                    .await
                {
                    tracing::error!(error = %e, id = self.id, "a sent message could not be marked");
                }
                account.sent(&text("category")).await;
                Ok(())
            }
            Err(e) if e.is_temporary() && !last => {
                account.failed(&e).await;
                Err(JobError::retry(e))
            }
            Err(e) => {
                account.failed(&e).await;
                if let Err(db_error) = fail(db, self.id, e.code, &e.message).await {
                    tracing::error!(error = %db_error, "the failed message could not be marked");
                }
                cx.events()
                    .emit(&MessageFailed {
                        id: self.id,
                        reason: e.message.clone(),
                    })
                    .await;
                Err(JobError::fail(e))
            }
        }
    }

    fn tries(&self) -> u32 {
        TRIES
    }

    /// One minute, then two, four and eight.
    fn backoff(&self, attempt: u32) -> Duration {
        Duration::from_secs(60 * 2u64.pow(attempt.saturating_sub(1).min(5)))
    }

    fn timeout(&self) -> Option<Duration> {
        Some(Duration::from_secs(45))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_needs_its_country_code() {
        assert_eq!(number("+91 98450 12345").as_deref(), Some("+919845012345"));
        assert_eq!(number("9845012345"), None);
        assert_eq!(number("+12"), None);
    }

    #[test]
    fn the_request_carries_body_values_then_the_button() {
        let request = body(
            "+919845012345",
            "order_ready",
            "en",
            &["Asha".to_string()],
            Some("A-100"),
        );
        assert_eq!(request["template"]["language"]["code"], "en");
        assert_eq!(
            request["template"]["components"][0]["parameters"][0]["text"],
            "Asha"
        );
        assert_eq!(request["template"]["components"][1]["sub_type"], "url");
        let bare = body("+919845012345", "hello", "en", &[], None);
        assert_eq!(bare["template"]["components"].as_array().unwrap().len(), 0);
    }
}
