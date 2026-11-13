//! The connection test: every part of the stored credential tried against
//! Meta with reads alone, so a wrong value is found before a message needs it.

use laterite_core::serde_json::Value;
use laterite_core::strata::async_trait;
use laterite_core::{t, Text};
use rainmill_vault::action::{Action, ActionCx, Outcome as ActionOutcome, Part, PartResult, State};

use crate::graph::{self, GraphError, Method, Request};

/// The permissions a token needs: to send, and to read and write templates.
const SCOPES: [&str; 2] = [
    "whatsapp_business_messaging",
    "whatsapp_business_management",
];

/// How one part of the setup fared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It works; the text says what Meta answered.
    Passed(String),
    /// It does not; the text says why.
    Failed(String),
    /// It is stored, and only a later event proves it.
    Later(String),
}

/// The Graph API version a credential names, else the plugin's own.
fn version_of(field: &dyn Fn(&str) -> String) -> String {
    match field("graph_version") {
        version if version.is_empty() => graph::DEFAULT_VERSION.to_string(),
        version => version,
    }
}

/// Subscribes the token's app to the business account, so Meta reports
/// deliveries and template reviews to the app's webhook. `field` reads the
/// credential.
pub async fn subscribe(field: &(dyn Fn(&str) -> String + Sync)) -> Result<(), GraphError> {
    graph::graph()
        .call(Request {
            method: Method::Post,
            version: version_of(field),
            path: format!("{}/subscribed_apps", field("business_account_id")),
            token: field("access_token"),
            body: None,
        })
        .await
        .map(|_| ())
}

/// The parts of the test, in the order the page shows them.
pub const PARTS: [&str; 6] = ["token", "secret", "account", "phone", "delivery", "webhook"];

/// What a part is called on the page.
pub fn part_label(part: &str) -> Text {
    match part {
        "token" => t!("Access token"),
        "secret" => t!("App secret"),
        "account" => t!("WhatsApp Business Account"),
        "phone" => t!("Phone number"),
        "delivery" => t!("Delivery reports"),
        "webhook" => t!("Webhook"),
        other => Text::dynamic(other.to_string()),
    }
}

/// One read of the Graph API with `token`.
async fn get(version: &str, path: String, token: String) -> Result<Value, GraphError> {
    graph::graph()
        .call(Request {
            method: Method::Get,
            version: version.to_string(),
            path,
            token,
            body: None,
        })
        .await
}

/// The account's numbers: the one call that needs the token, the account and
/// the token's reach over it all at once.
async fn numbers(version: &str, account: &str, token: &str) -> Result<Value, GraphError> {
    get(
        version,
        format!("{account}/phone_numbers?fields=id,display_phone_number,verified_name&limit=100"),
        token.to_string(),
    )
    .await
}

/// Tries one part of a credential, read through `field`, with reads alone:
/// no message is sent. Each part makes its own calls, so the parts run in any
/// order or at once.
pub async fn run_part(field: &(dyn Fn(&str) -> String + Sync), part: &str) -> Outcome {
    let token = field("access_token");
    let account = field("business_account_id");
    let secret = field("app_secret");
    let version = version_of(field);
    let describe = || {
        get(
            &version,
            format!("debug_token?input_token={token}"),
            token.clone(),
        )
    };
    if token.is_empty() && part != "webhook" {
        return match part {
            "token" => Outcome::Failed("No access token is stored.".to_string()),
            "secret" if secret.is_empty() => Outcome::Failed(
                "No app secret is stored, so no delivery report can be trusted.".to_string(),
            ),
            _ => Outcome::Later("It could not be tried without an access token.".to_string()),
        };
    }
    match part {
        // The token, as Meta describes it.
        "token" => match describe().await {
            Ok(answer) => token_outcome(&answer),
            // Meta will not always describe a token to itself.
            Err(e) => match numbers(&version, &account, &token).await {
                Ok(_) if !account.is_empty() => Outcome::Passed(format!(
                    "Meta takes it for the account. Its permissions and expiry could not be read: {}",
                    e.message
                )),
                _ => Outcome::Failed(e.message),
            },
        },
        // The app secret: an app's own token is its id and its secret.
        "secret" => {
            if secret.is_empty() {
                return Outcome::Failed(
                    "No app secret is stored, so no delivery report can be trusted.".to_string(),
                );
            }
            let app_id = match describe().await {
                Ok(answer) => text(&answer, "/data/app_id"),
                Err(_) => String::new(),
            };
            if app_id.is_empty() {
                return Outcome::Later(
                    "Stored. It could not be tried without a working token.".to_string(),
                );
            }
            match get(
                &version,
                format!("debug_token?input_token={token}"),
                format!("{app_id}|{secret}"),
            )
            .await
            {
                Ok(_) => Outcome::Passed(format!("Belongs to app {app_id}.")),
                Err(e) => {
                    Outcome::Failed(format!("Meta refused it for app {app_id}: {}", e.message))
                }
            }
        }
        "account" if account.is_empty() => Outcome::Failed("No account id is stored.".to_string()),
        "account" => match numbers(&version, &account, &token).await {
            Ok(_) => Outcome::Passed(format!("Account {account} answers.")),
            // A permission refusal means the token lacks reach over the account.
            Err(e) if matches!(e.code, Some(10 | 100 | 200)) => Outcome::Failed(format!(
                "{} The token's system user needs this WhatsApp account assigned to it, with the permission whatsapp_business_management.",
                e.message
            )),
            Err(e) => Outcome::Failed(e.message),
        },
        "phone" => match numbers(&version, &account, &token).await {
            Ok(answer) if !account.is_empty() => {
                number_outcome(&answer, &field("phone_number_id"))
            }
            _ => Outcome::Later("The account's numbers could not be read.".to_string()),
        },
        // Without a subscribed app Meta reports no delivery to anyone.
        "delivery" if account.is_empty() => {
            Outcome::Later("It could not be tried without an account id.".to_string())
        }
        "delivery" => match get(&version, format!("{account}/subscribed_apps"), token.clone()).await
        {
            Ok(answer) => subscription_outcome(&answer),
            Err(e) => Outcome::Later(format!(
                "Who receives the account's deliveries could not be read: {}",
                e.message
            )),
        },
        // The webhook, as the app itself has it: Meta saves one only after the
        // verify token matched, so a saved one proves the token too.
        "webhook" => {
            if field("verify_token").is_empty() {
                return Outcome::Failed(
                    "No verify token is stored, so Meta cannot set the webhook.".to_string(),
                );
            }
            let unread = |why: &str| {
                Outcome::Later(format!(
                    "A verify token is stored. Whether Meta has the webhook could not be read: {why}"
                ))
            };
            if token.is_empty() || secret.is_empty() {
                return unread("it takes a working token and app secret.");
            }
            let app_id = match describe().await {
                Ok(answer) => text(&answer, "/data/app_id"),
                Err(e) => return unread(&e.message),
            };
            match get(
                &version,
                format!("{app_id}/subscriptions"),
                format!("{app_id}|{secret}"),
            )
            .await
            {
                Ok(answer) => webhook_outcome(&answer),
                Err(e) => unread(&e.message),
            }
        }
        _ => Outcome::Later(String::new()),
    }
}

/// The text at `pointer`, a number read as text.
fn text(answer: &Value, pointer: &str) -> String {
    match answer.pointer(pointer) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

/// Whether the token is valid, lasts, and carries both permissions.
fn token_outcome(answer: &Value) -> Outcome {
    if answer.pointer("/data/is_valid").and_then(Value::as_bool) == Some(false) {
        let why = text(answer, "/data/error/message");
        return Outcome::Failed(if why.is_empty() {
            "Meta says the token is not valid.".to_string()
        } else {
            why
        });
    }
    let held: Vec<&str> = answer
        .pointer("/data/scopes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let missing: Vec<&str> = SCOPES
        .into_iter()
        .filter(|scope| !held.contains(scope))
        .collect();
    if !missing.is_empty() {
        return Outcome::Failed(format!(
            "The token lacks the permission {}.",
            missing.join(" and ")
        ));
    }
    let expires = answer
        .pointer("/data/expires_at")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let lasts = match chrono::DateTime::from_timestamp(expires, 0) {
        Some(at) if expires > 0 => format!("expires {}", at.format("%-d %B %Y")),
        _ => "never expires".to_string(),
    };
    Outcome::Passed(format!(
        "Valid for app {}, {lasts}, with both WhatsApp permissions.",
        text(answer, "/data/app_id")
    ))
}

/// Whether the stored number is one of the account's.
fn number_outcome(answer: &Value, phone: &str) -> Outcome {
    let numbers: Vec<&Value> = answer
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    if phone.is_empty() {
        return Outcome::Failed("No phone number id is stored.".to_string());
    }
    match numbers.iter().find(|n| text(n, "/id") == phone) {
        Some(number) => Outcome::Passed(format!(
            "{} ({}).",
            text(number, "/display_phone_number"),
            text(number, "/verified_name")
        )),
        None => {
            let known: Vec<String> = numbers
                .iter()
                .map(|n| format!("{} is {}", text(n, "/id"), text(n, "/display_phone_number")))
                .collect();
            Outcome::Failed(if known.is_empty() {
                "The account has no phone number.".to_string()
            } else {
                format!(
                    "The account has no number with the id {phone}. It has: {}.",
                    known.join("; ")
                )
            })
        }
    }
}

/// Where the webhook lives on the application, and what Meta must send it.
const WEBHOOK_PATH: &str = "/webhooks/whatsapp";
const WEBHOOK_FIELDS: [&str; 2] = ["messages", "message_template_status_update"];

/// Whether the app's WhatsApp webhook is set, on, pointed here and sending
/// what the plugin reads.
fn webhook_outcome(answer: &Value) -> Outcome {
    let hook = answer
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|hook| text(hook, "/object") == "whatsapp_business_account");
    let Some(hook) = hook else {
        return Outcome::Failed(format!(
            "The Meta app has no WhatsApp webhook. Set one in the app under WhatsApp > Configuration, with a callback URL ending in {WEBHOOK_PATH}."
        ));
    };
    let url = text(hook, "/callback_url");
    if !url.trim_end_matches('/').ends_with(WEBHOOK_PATH) {
        return Outcome::Failed(format!(
            "Meta delivers to {url}, which is not this application's webhook. The callback URL ends in {WEBHOOK_PATH}."
        ));
    }
    if hook.get("active").and_then(Value::as_bool) == Some(false) {
        return Outcome::Failed(format!(
            "The webhook to {url} is switched off in the Meta app."
        ));
    }
    let sent: Vec<String> = hook
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|field| text(field, "/name"))
        .collect();
    let missing: Vec<&str> = WEBHOOK_FIELDS
        .into_iter()
        .filter(|field| !sent.iter().any(|s| s == field))
        .collect();
    if !missing.is_empty() {
        return Outcome::Failed(format!(
            "Meta delivers to {url} but does not send {}. Subscribe to it under the webhook's fields in the Meta app.",
            missing.join(" or ")
        ));
    }
    Outcome::Passed(format!(
        "Meta delivers to {url}, with messages and template reviews. The verify token matched when it was saved."
    ))
}

/// Whether an app is subscribed to the account.
fn subscription_outcome(answer: &Value) -> Outcome {
    let apps: Vec<String> = answer
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|app| text(app, "/whatsapp_business_api_data/name"))
        .collect();
    if apps.is_empty() {
        Outcome::Failed(
            "No app is subscribed to this account, so Meta reports no delivery.".to_string(),
        )
    } else {
        Outcome::Passed(format!("Sent to the app {}.", apps.join(", ")))
    }
}

/// The credential's fields, trimmed, as the checks read them.
fn reader<'a>(cx: &'a ActionCx<'_>) -> impl Fn(&str) -> String + Sync + 'a {
    move |name| cx.field(name).unwrap_or_default().trim().to_string()
}

/// Test connection, on the credential's page: one row a part, each tried on
/// its own.
pub(crate) struct Test;

#[async_trait]
impl Action for Test {
    fn parts(&self) -> Vec<Part> {
        PARTS
            .into_iter()
            .map(|part| Part::new(part, part_label(part)))
            .collect()
    }

    fn note(&self) -> Option<Text> {
        Some(t!(
            "Each part of the credential, tried against Meta. No message is sent."
        ))
    }

    async fn part(&self, cx: &ActionCx<'_>, code: &str) -> PartResult {
        let outcome = run_part(&reader(cx), code).await;
        // The account answering is Meta taking the credential; an account
        // with no app subscribed is mended by subscribing one.
        let accepted = code == "account" && matches!(outcome, Outcome::Passed(_));
        let unsubscribed = code == "delivery" && matches!(outcome, Outcome::Failed(_));
        let (state, detail) = match outcome {
            Outcome::Passed(detail) => (State::Works, detail),
            Outcome::Failed(detail) => (State::Failed, detail),
            Outcome::Later(detail) => (State::NotTried, detail),
        };
        let result = PartResult::new(state, detail);
        let result = if accepted { result.accepted() } else { result };
        if unsubscribed {
            result.offer(SUBSCRIBE)
        } else {
            result
        }
    }
}

/// Subscribe the app, offered by the test when no app receives deliveries.
pub(crate) struct Subscribe;

#[async_trait]
impl Action for Subscribe {
    async fn run(&self, cx: &ActionCx<'_>) -> ActionOutcome {
        match subscribe(&reader(cx)).await {
            Ok(()) => ActionOutcome::Done(t!(
                "The app is subscribed to the account. Test the connection again to see it."
            )),
            Err(e) => ActionOutcome::Failed(Text::dynamic(format!(
                "The app could not be subscribed: {}",
                e.message
            ))),
        }
    }
}

/// The code of the test's action.
pub(crate) const TEST: &str = "test";
/// The code of the subscribe action.
pub(crate) const SUBSCRIBE: &str = "subscribe";

#[cfg(test)]
mod tests {
    use super::*;
    use laterite_core::serde_json::json;

    #[test]
    fn a_token_is_judged_by_validity_permissions_and_expiry() {
        let good = json!({ "data": { "app_id": "42", "is_valid": true, "expires_at": 0,
            "scopes": ["whatsapp_business_messaging", "whatsapp_business_management"] } });
        assert_eq!(
            token_outcome(&good),
            Outcome::Passed(
                "Valid for app 42, never expires, with both WhatsApp permissions.".to_string()
            )
        );
        let narrow = json!({ "data": { "app_id": "42", "is_valid": true,
            "scopes": ["whatsapp_business_management"] } });
        assert_eq!(
            token_outcome(&narrow),
            Outcome::Failed(
                "The token lacks the permission whatsapp_business_messaging.".to_string()
            )
        );
        let dead =
            json!({ "data": { "is_valid": false, "error": { "message": "Session expired." } } });
        assert_eq!(
            token_outcome(&dead),
            Outcome::Failed("Session expired.".to_string())
        );
    }

    #[test]
    fn a_webhook_must_point_here_be_on_and_send_both_fields() {
        let hook = |url: &str, fields: &[&str]| {
            json!({ "data": [{ "object": "whatsapp_business_account", "callback_url": url, "active": true,
                "fields": fields.iter().map(|f| json!({ "name": f })).collect::<Vec<_>>() }] })
        };
        let both = ["messages", "message_template_status_update"];
        assert!(matches!(
            webhook_outcome(&hook("https://acme.test/webhooks/whatsapp", &both)),
            Outcome::Passed(_)
        ));
        assert!(matches!(
            webhook_outcome(&hook("https://other.test/hook", &both)),
            Outcome::Failed(_)
        ));
        assert_eq!(
            webhook_outcome(&hook("https://acme.test/webhooks/whatsapp", &["messages"])),
            Outcome::Failed(
                "Meta delivers to https://acme.test/webhooks/whatsapp but does not send message_template_status_update. Subscribe to it under the webhook's fields in the Meta app."
                    .to_string()
            )
        );
        assert!(matches!(
            webhook_outcome(&json!({ "data": [] })),
            Outcome::Failed(_)
        ));
    }

    #[test]
    fn a_number_must_be_one_of_the_accounts() {
        let answer = json!({ "data": [
            { "id": "555", "display_phone_number": "+1 555 0100", "verified_name": "Acme" }
        ] });
        assert_eq!(
            number_outcome(&answer, "555"),
            Outcome::Passed("+1 555 0100 (Acme).".to_string())
        );
        assert_eq!(
            number_outcome(&answer, "556"),
            Outcome::Failed(
                "The account has no number with the id 556. It has: 555 is +1 555 0100."
                    .to_string()
            )
        );
    }
}
