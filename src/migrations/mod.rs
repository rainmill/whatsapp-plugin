//! The plugin's schema, as portable one-file migrations. Append new entries at
//! the end; never reorder or rename a shipped one.

laterite_core::migration_set! {
    module_id: "rainmill.whatsapp",
    m0001_create_whatsapp,
}
