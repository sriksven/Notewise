# Notification service

> **Status: scaffold.** BSL 1.1 — see [LICENSE-CLOUD.md](../../LICENSE-CLOUD.md).

Push, Slack, and email delivery.

**Default to digest mode, not real-time.** Notification fatigue kills adoption faster than
almost anything else in this product; real-time should be opt-in per notification type.

The `Notification` model and its channel/delivery state already exist in
`core/crates/storage`.
