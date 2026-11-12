//! Table and column identifiers, as `sea-query` `Iden` enums. The enum name
//! becomes the table name in snake_case.

use laterite_core::sea_query;
use sea_query::Iden;

/// A message template, as Meta last reported it.
#[derive(Iden)]
pub(crate) enum WhatsappTemplates {
    Table,
    Id,
    /// Meta's id for the template.
    WaId,
    Name,
    Language,
    Category,
    Status,
    RejectedReason,
    /// The components as Meta returns them, a JSON array.
    Components,
    SyncedAt,
}

/// A message this application sent, and what became of it.
#[derive(Iden)]
pub(crate) enum WhatsappMessages {
    Table,
    Id,
    /// Meta's id for the message, once it accepted it.
    WaMessageId,
    ToNumber,
    Template,
    Language,
    Category,
    /// The body parameters, a JSON array of strings.
    Params,
    /// The URL button's parameter, when the template has one.
    ButtonParam,
    /// queued, sent, delivered, read or failed.
    State,
    ErrorCode,
    ErrorText,
    /// Whether Meta charged for it: 1, 0, or null while unknown.
    Billable,
    PricingCategory,
    /// The application record the message is about.
    SubjectType,
    SubjectId,
    CreatedAt,
    SentAt,
    DeliveredAt,
    ReadAt,
    FailedAt,
}
