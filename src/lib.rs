//! Rainmill WhatsApp: WhatsApp Business messaging for a Laterite application,
//! over Meta's Cloud API.
//!
//! The access token lives in the vault (`rainmill-vault`), stored on the
//! Credentials screen as a "WhatsApp Cloud API" credential. The plugin mirrors
//! the account's message templates so the admin can list and preview them.
//!
//! ```ignore
//! let held = rainmill_whatsapp::templates::sync(db, crypt).await?;
//! ```

mod admin;
pub mod check;
pub mod graph;
pub mod messages;
pub mod migrations;
mod schema;
pub mod templates;
pub mod webhook;

use laterite_admin::form::FormField;
use laterite_core::{t, Crypt, Db, MigrationSet, Module, ModuleId, Registry};
use rainmill_vault::{KindReg, Lease, SlotReg, VaultError};

pub use graph::{use_graph, Graph, GraphError};
pub use messages::{send, Message, MessageFailed};
pub use templates::{Draft, Preview, Template};

/// The kind of credential the plugin reads from the vault.
pub const KIND: &str = "rainmill.whatsapp.cloud";
/// The slot the credential is attached to.
pub const SLOT: &str = "rainmill.whatsapp.cloud";
/// The permission to see templates, sync them and submit new ones.
pub const TEMPLATES: &str = "rainmill.whatsapp.templates";
/// The permission to read the message log.
pub const MESSAGES: &str = "rainmill.whatsapp.messages";

/// What can go wrong talking to WhatsApp.
#[derive(Debug, thiserror::Error)]
pub enum WhatsAppError {
    /// No usable credential is attached.
    #[error("{0}")]
    Vault(#[from] VaultError),
    /// The credential is missing something a call needs.
    #[error("the WhatsApp credential has no {0}")]
    Incomplete(&'static str),
    /// Meta refused the call or could not be reached.
    #[error("{0}")]
    Graph(#[from] GraphError),
    /// The message cannot be sent as asked.
    #[error("{0}")]
    Refused(String),
    /// The database failed.
    #[error("the database failed: {0}")]
    Store(#[from] laterite_core::sqlx::Error),
}

/// The WhatsApp account a call is made for, read from the vault for one use.
pub(crate) struct Account {
    lease: Lease,
    pub(crate) token: String,
    pub(crate) phone_number_id: String,
    pub(crate) business_account_id: String,
    pub(crate) version: String,
}

impl Account {
    /// Takes the credential attached to the plugin's slot.
    pub(crate) async fn lease(db: &Db, crypt: &Crypt) -> Result<Self, WhatsAppError> {
        let lease = rainmill_vault::lease(db, crypt, SLOT).await?;
        let field = |name: &str| lease.field(name).unwrap_or_default().trim().to_string();
        let need = |name: &'static str| match field(name) {
            value if value.is_empty() => Err(WhatsAppError::Incomplete(name)),
            value => Ok(value),
        };
        Ok(Self {
            token: need("access_token")?,
            phone_number_id: need("phone_number_id")?,
            business_account_id: need("business_account_id")?,
            version: match field("graph_version") {
                version if version.is_empty() => graph::DEFAULT_VERSION.to_string(),
                version => version,
            },
            lease,
        })
    }

    /// The call worked and sent no message.
    pub(crate) async fn used(self) {
        if let Err(e) = self.lease.used(0).await {
            tracing::warn!(error = %e, "the use could not be recorded");
        }
    }

    /// The call sent one message of `category`.
    pub(crate) async fn sent(self, category: &str) {
        let meter = category.to_ascii_lowercase();
        if let Err(e) = self.lease.used_as(&meter, 1).await {
            tracing::warn!(error = %e, "the message could not be counted");
        }
    }

    /// The call failed; a refusal of the token itself is passed on to the vault.
    pub(crate) async fn failed(self, error: &GraphError) {
        if let Err(e) = self.lease.failed(error.status, &error.message).await {
            tracing::warn!(error = %e, "the failed use could not be recorded");
        }
    }
}

/// This plugin's entry point. Every Laterite plugin exposes `module()`, so the
/// generated plugin manifest collects it without naming the type.
pub fn module() -> Box<dyn Module> {
    Box::new(WhatsAppModule)
}

/// The plugin as a Laterite module.
pub struct WhatsAppModule;

#[laterite_core::strata::async_trait]
impl Module for WhatsAppModule {
    fn id(&self) -> ModuleId {
        ModuleId::new(migrations::MODULE_ID)
    }

    fn requires(&self) -> &'static [ModuleId] {
        const REQUIRES: &[ModuleId] = &[ModuleId::new("rainmill.vault")];
        REQUIRES
    }

    fn migrations(&self) -> MigrationSet {
        migrations::migrations()
    }

    fn register(&self, registry: &mut Registry) {
        registry.add(
            KindReg::new(KIND, t!("WhatsApp Cloud API"))
                .category(rainmill_vault::Category::Messaging)
                .help(t!(
                    "From Meta Business settings: a system user's permanent token with the WhatsApp permissions, and the ids shown under WhatsApp Manager > Phone numbers."
                ))
                .field(FormField::of("access_token", t!("Access token"), "sensitive").required())
                .field(FormField::text("phone_number_id", t!("Phone number ID")).required())
                .field(
                    FormField::text("business_account_id", t!("WhatsApp Business Account ID"))
                        .required(),
                )
                .field(FormField::of("app_secret", t!("App secret"), "sensitive").help(t!(
                    "Checks that a delivery report came from Meta. From the Meta app's Basic settings."
                )))
                .field(FormField::of("verify_token", t!("Webhook verify token"), "sensitive").help(t!(
                    "Any phrase you choose. Enter the same one where Meta asks for it when you set the webhook."
                )))
                .field(FormField::text("graph_version", t!("Graph API version")).help(t!(
                    "Leave blank for the version this plugin was written for."
                ))),
        );
        registry.add(
            SlotReg::single(SLOT, KIND)
                .label(t!("WhatsApp messages"))
                .reason(t!(
                    "Sends template messages and reads the account's templates."
                ))
                .unit(t!("Messages sent"))
                .meter("marketing", t!("Marketing"))
                .meter("utility", t!("Utility"))
                .meter("authentication", t!("Authentication"))
                .meter("service", t!("Service")),
        );
        registry.add(
            rainmill_vault::ActionReg::new(
                KIND,
                check::TEST,
                t!("Test connection"),
                std::sync::Arc::new(check::Test),
            )
            .icon("plug"),
        );
        registry.add(
            rainmill_vault::ActionReg::new(
                KIND,
                check::SUBSCRIBE,
                t!("Subscribe the app"),
                std::sync::Arc::new(check::Subscribe),
            )
            .unlisted(),
        );
        laterite_core::queue::QueueRegistry::add_job::<messages::SendMessage>(registry);
        admin::register(registry);
        webhook::register(registry);
    }
}
