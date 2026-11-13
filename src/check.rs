//! The connection test: every part of the stored credential tried against
//! Meta with reads alone, so a wrong value is found before a message needs it.

use laterite_core::serde_json::Value;
use laterite_core::{t, Crypt, Db, Text};

use crate::graph::{self, Method, Request};
use crate::SLOT;

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

/// One line of the test.
#[derive(Clone, Debug)]
pub struct Check {
    /// What was tried.
    pub label: Text,
    /// What came of it.
    pub outcome: Outcome,
    /// Whether [`subscribe`] mends it.
    pub subscribable: bool,
}

impl Check {
    fn new(label: Text, outcome: Outcome) -> Self {
        Self {
            label,
            outcome,
            subscribable: false,
        }
    }
}

/// Subscribes the token's app to the business account, so Meta reports
/// deliveries and template reviews to the app's webhook.
pub async fn subscribe(db: &Db, crypt: &Crypt) -> Result<(), crate::WhatsAppError> {
    let account = crate::Account::lease(db, crypt).await?;
    let path = format!("{}/subscribed_apps", account.business_account_id);
    match account.call(Method::Post, &path, None).await {
        Ok(_) => {
            account.used().await;
            Ok(())
        }
        Err(e) => {
            account.failed(&e).await;
            Err(e.into())
        }
    }
}

/// Tries the credential attached to the plugin's slot: the token and what it
/// may do, the app secret, the business account, the phone number, and whether
/// an app receives the account's deliveries. Sends no message.
pub async fn run(db: &Db, crypt: &Crypt) -> Vec<Check> {
    let lease = match rainmill_vault::inspect(db, crypt, SLOT).await {
        Ok(lease) => lease,
        Err(e) => return vec![Check::new(t!("Credential"), Outcome::Failed(format!(
                    "No credential could be used ({e}). Store one under Credentials and attach it to WhatsApp messages."
                )))],
    };
    let field = |name: &str| lease.field(name).unwrap_or_default().trim().to_string();
    let token = field("access_token");
    let phone = field("phone_number_id");
    let account = field("business_account_id");
    let secret = field("app_secret");
    let version = match field("graph_version") {
        version if version.is_empty() => graph::DEFAULT_VERSION.to_string(),
        version => version,
    };
    let get = |path: String, token: String| {
        let version = version.clone();
        async move {
            graph::graph()
                .call(Request {
                    method: Method::Get,
                    version,
                    path,
                    token,
                    body: None,
                })
                .await
        }
    };
    let mut checks = Vec::new();

    // The account's numbers: the one call that needs the token, the account
    // and the token's reach over it all at once.
    let numbers = if token.is_empty() || account.is_empty() {
        None
    } else {
        Some(
            get(
                format!(
                    "{account}/phone_numbers?fields=id,display_phone_number,verified_name&limit=100"
                ),
                token.clone(),
            )
            .await,
        )
    };
    let reached = matches!(numbers, Some(Ok(_)));

    // The token, as Meta describes it.
    let mut app_id = String::new();
    if token.is_empty() {
        checks.push(Check::new(
            t!("Access token"),
            Outcome::Failed("No access token is stored.".to_string()),
        ));
    } else {
        let described = get(format!("debug_token?input_token={token}"), token.clone()).await;
        checks.push(Check::new(
            t!("Access token"),
            match &described {
                Ok(answer) => {
                    app_id = text(answer, "/data/app_id");
                    token_outcome(answer)
                }
                // Meta will not always describe a token to itself.
                Err(e) if reached => Outcome::Passed(format!(
                    "Meta takes it for the account. Its permissions and expiry could not be read: {}",
                    e.message
                )),
                Err(e) => Outcome::Failed(e.message.clone()),
            },
        ));
    }

    // The app secret: an app's own token is its id and its secret.
    checks.push(Check::new(
        t!("App secret"),
        if secret.is_empty() {
            Outcome::Failed(
                "No app secret is stored, so no delivery report can be trusted.".to_string(),
            )
        } else if app_id.is_empty() {
            Outcome::Later("Stored. It could not be tried without a working token.".to_string())
        } else {
            match get(
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
        },
    ));

    // The business account and the number under it.
    if account.is_empty() {
        checks.push(Check::new(
            t!("WhatsApp Business Account"),
            Outcome::Failed("No account id is stored.".to_string()),
        ));
    }
    if let Some(numbers) = numbers {
        match numbers {
            Ok(answer) => {
                checks.push(Check::new(
                    t!("WhatsApp Business Account"),
                    Outcome::Passed(format!("Account {account} answers.")),
                ));
                checks.push(Check::new(
                    t!("Phone number"),
                    number_outcome(&answer, &phone),
                ));
            }
            Err(e) => checks.push(Check::new(
                t!("WhatsApp Business Account"),
                Outcome::Failed(format!(
                    "{} The token's system user needs this WhatsApp account assigned to it, with the permission whatsapp_business_management.",
                    e.message
                )),
            )),
        }
        // Without a subscribed app Meta reports no delivery to anyone.
        let subscribed = get(format!("{account}/subscribed_apps"), token.clone()).await;
        let mut check = Check::new(
            t!("Delivery reports"),
            match &subscribed {
                Ok(answer) => subscription_outcome(answer),
                Err(e) => Outcome::Failed(e.message.clone()),
            },
        );
        check.subscribable = subscribed.is_ok() && matches!(check.outcome, Outcome::Failed(_));
        checks.push(check);
    }

    checks.push(Check::new(
        t!("Webhook verify token"),
        if field("verify_token").is_empty() {
            Outcome::Failed(
                "No verify token is stored, so Meta cannot set the webhook.".to_string(),
            )
        } else {
            Outcome::Later("Stored. Meta tries it when the webhook is saved.".to_string())
        },
    ));

    // A test never puts the credential out of use; one Meta took for the
    // account puts it back in good standing.
    if reached {
        if let Err(e) = lease.used(0).await {
            tracing::warn!(error = %e, "the test could not be recorded");
        }
    }
    checks
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
