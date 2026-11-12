//! Create the template mirror and the message log. Portable SQL only:
//! auto-increment integer ids and text timestamps.

use laterite_core::strata::*;

use crate::schema::{WhatsappMessages as Messages, WhatsappTemplates as Templates};

pub struct Migration;

#[async_trait(?Send)]
impl laterite_core::Migration for Migration {
    fn name(&self) -> &str {
        "0001_create_whatsapp"
    }

    async fn up(&self, s: &mut Schema<'_>) -> CoreResult<()> {
        fn id(col: impl Iden + 'static) -> ColumnDef {
            ColumnDef::new(col)
                .big_integer()
                .not_null()
                .auto_increment()
                .primary_key()
                .to_owned()
        }
        s.exec(
            Table::create()
                .table(Templates::Table)
                .if_not_exists()
                .col(id(Templates::Id))
                .col(key_col(Templates::WaId).not_null())
                .col(key_col(Templates::Name).not_null())
                .col(key_col(Templates::Language).not_null())
                .col(key_col(Templates::Category).not_null())
                .col(key_col(Templates::Status).not_null())
                .col(ColumnDef::new(Templates::RejectedReason).text())
                .col(ColumnDef::new(Templates::Components).text().not_null())
                .col(ColumnDef::new(Templates::SyncedAt).text().not_null())
                .to_owned(),
        )
        .await?;
        s.exec(
            Index::create()
                .name("whatsapp_templates_name_language_idx")
                .table(Templates::Table)
                .col(Templates::Name)
                .col(Templates::Language)
                .unique()
                .to_owned(),
        )
        .await?;
        s.exec(
            Table::create()
                .table(Messages::Table)
                .if_not_exists()
                .col(id(Messages::Id))
                .col(key_col(Messages::WaMessageId))
                .col(key_col(Messages::ToNumber).not_null())
                .col(key_col(Messages::Template).not_null())
                .col(key_col(Messages::Language).not_null())
                .col(key_col(Messages::Category).not_null())
                .col(ColumnDef::new(Messages::Params).text().not_null())
                .col(ColumnDef::new(Messages::ButtonParam).text())
                .col(key_col(Messages::State).not_null().default("queued"))
                .col(key_col(Messages::ErrorCode))
                .col(ColumnDef::new(Messages::ErrorText).text())
                .col(ColumnDef::new(Messages::Billable).integer())
                .col(key_col(Messages::PricingCategory))
                .col(key_col(Messages::SubjectType))
                .col(ColumnDef::new(Messages::SubjectId).big_integer())
                .col(ColumnDef::new(Messages::CreatedAt).text().not_null())
                .col(ColumnDef::new(Messages::SentAt).text())
                .col(ColumnDef::new(Messages::DeliveredAt).text())
                .col(ColumnDef::new(Messages::ReadAt).text())
                .col(ColumnDef::new(Messages::FailedAt).text())
                .to_owned(),
        )
        .await?;
        s.exec(
            Index::create()
                .name("whatsapp_messages_wa_message_id_idx")
                .table(Messages::Table)
                .col(Messages::WaMessageId)
                .to_owned(),
        )
        .await?;
        s.exec(
            Index::create()
                .name("whatsapp_messages_created_at_idx")
                .table(Messages::Table)
                .col(Messages::CreatedAt)
                .to_owned(),
        )
        .await
    }

    async fn down(&self, s: &mut Schema<'_>) -> CoreResult<()> {
        s.exec(Table::drop().table(Messages::Table).to_owned()).await?;
        s.exec(Table::drop().table(Templates::Table).to_owned()).await
    }
}
