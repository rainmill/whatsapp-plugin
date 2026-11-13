# Changelog

Notable changes to `rainmill-whatsapp`. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/) as Cargo reads it: before 1.0 a minor
bump is breaking and a patch is additive.

## [Unreleased]

### Added

- First cut. WhatsApp Business messaging for Laterite over Meta's Cloud API: the account's message templates mirrored and previewed in the admin, template messages sent through the queue with retries, templates written, previewed and submitted for review from the admin, a signed webhook that records sent, delivered, read and failed, and a Messages screen with figures and charts by outcome, category and template.
- Test connection on the Templates screen tries the stored credential against Meta with reads alone: the token and its permissions, the app secret, the account, the phone number and the delivery subscription.
