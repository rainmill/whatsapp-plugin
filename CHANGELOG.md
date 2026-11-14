# Changelog

Notable changes to `rainmill-whatsapp`. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/) as Cargo reads it: before 1.0 a minor
bump is breaking and a patch is additive.

## [Unreleased]

### Added

- First cut. WhatsApp Business messaging for Laterite over Meta's Cloud API: the account's message templates mirrored and previewed in the admin, template messages sent through the queue with retries, templates written, previewed and submitted for review from the admin, a signed webhook that records sent, delivered, read and failed, and a Messages screen with figures and charts by outcome, category and template.
- Test connection on a WhatsApp credential's page tries it against Meta with reads alone: the token and its permissions, the app secret, the account, the phone number, the delivery subscription and the app's webhook. It offers Subscribe the app when no app receives the account's deliveries.
- The Templates screens draw a message as a phone shows it, count each template's sent, delivered, read, failed and charged messages, and filter by name, category and status. New template previews as it is typed and takes one sample a value.
- A credential's own webhook address: Subscribe the app has Meta deliver that account's reports there, for a Meta app whose webhook belongs to another application.
