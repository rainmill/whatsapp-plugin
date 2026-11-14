# Rainmill WhatsApp

WhatsApp Business messaging for a [Laterite](https://laterite.rs) application, over Meta's Cloud API.

- **Templates**: the account's message templates, mirrored from Meta, with their review status and a preview of each as a recipient reads it.
- **Sending**: a template message is queued, sent, and retried when Meta is unreachable.
- **Tracking**: every message is logged and moves to sent, delivered, read or failed as Meta reports it.
- **Usage**: each message is counted by category (marketing, utility, authentication, service) in [Rainmill Vault](https://github.com/rainmill/vault-plugin), which charts it and estimates the cost.

## Requirements

Laterite 0.11 and `rainmill-vault`. A WhatsApp Business Account with a phone number on the Cloud API.

## Installation

```toml
[dependencies]
rainmill-whatsapp = "0.1"
```

Add `rainmill_whatsapp::module()` to the application's modules, after `rainmill_vault::module()`.

## Store the credential

Under Settings, Credentials, add a **WhatsApp Cloud API** credential and switch on **WhatsApp messages**.

| Field | Where it comes from |
| --- | --- |
| Access token | Meta Business settings: a system user's permanent token with `whatsapp_business_messaging` and `whatsapp_business_management` |
| Phone number ID | WhatsApp Manager, Phone numbers |
| WhatsApp Business Account ID | WhatsApp Manager, Phone numbers |
| App secret | The Meta app's Basic settings |
| Webhook verify token | Any phrase you choose |
| Graph API version | Blank for `v23.0` |

## Set the webhook

In the Meta app, under WhatsApp, Configuration, set the callback URL to `https://<your site>/webhooks/whatsapp` and the verify token to the phrase stored above, then subscribe to `messages` and `message_template_status_update`.

## Write a template

Templates lists each template with its category, its review status and, for the last 30 days, how many messages were sent, delivered, read, failed and charged. A template's page shows it as a phone does, with the sample values or as written, the values it takes and its latest messages.

Templates, New template: a name, a category, the message with `{{1}}`, `{{2}}` where values go, and a sample of each value. The preview beside the form follows what is typed. Check names anything Meta's rules refuse; Submit to Meta for review then sends it, and the list shows it in review until Meta decides. Delete at Meta removes one language of a template.

## Send a message

```rust
use rainmill_whatsapp::{send, Message};

let id = send(
    &queue,
    &db,
    Message::template("order_ready", "en")
        .to("+919845012345")
        .param("Asha")
        .param("A-100")
        .button("A-100")
        .about("orders", 42),
)
.await?;
```

| Call | Effect |
| --- | --- |
| `Message::template(name, language)` | The template, as the Templates screen lists it. |
| `.to(number)` | The recipient, with the country code. |
| `.param(value)` | The next body value: `{{1}}`, then `{{2}}`. |
| `.button(value)` | The value the template's URL button ends in. |
| `.about(entity, id)` | The application record the message is about. |

`send` answers `WhatsAppError::Refused` and queues nothing when the number has no country code, the template is not approved, or the values do not match the template.

## When Meta fails

A message stays queued and is tried five times over fifteen minutes when Meta is unreachable or answers with a server error. A message Meta refuses is marked failed with Meta's code and words, shown on the Messages screen, and announced as the event `rainmill.whatsapp.message.failed` (`MessageFailed { id, reason }`), which an application listens to when it has a second channel.

## Permissions

| Permission | Grants |
| --- | --- |
| `rainmill.whatsapp.templates` | The Templates screen: Sync now, New template, Delete at Meta |
| `rainmill.whatsapp.messages` | The Messages screen |

## License

MIT OR Apache-2.0.
