# Task 06 — Outbound/Inbound Webhooks & Transactional Email

## Goal

Implement outbound webhook delivery (`Outbox`/`JobQueue` integration, HMAC-SHA256 `X-Rume-Signature`, exponential backoff, delivery log) and inbound webhook signature + replay verification middleware, plus transactional email (`Mailer` trait, feature-gated `SmtpMailer` via `lettre` + `minijinja`, and `TestMailer` in `rume-testing`).

## Files touched

- `crates/messaging/src/webhooks/mod.rs` (and submodules `signer.rs`, `dispatcher.rs`, `store.rs`)
- `crates/http/src/webhooks/mod.rs`
- `crates/messaging/src/mailer/mod.rs` (and submodules `smtp.rs`, `template.rs`)
- `crates/testing/src/mailer.rs`, `crates/testing/src/lib.rs`

## Deliverable

- **Outbound Webhooks (`rume-messaging::webhooks`)**:
  - `WebhookEndpoint` (`id`, `tenant_id`, `url`, `secret: SecretString`, `events`, `active`).
  - `sign_webhook_payload(secret, timestamp, body) -> String` producing `t=<ts>,v1=<hex_hmac_sha256>` over `"{ts}.{body}"`.
  - `WebhookDeliveryStore` trait + `InMemoryWebhookDeliveryStore` + `SqlWebhookDeliveryStore` (`rume_webhook_deliveries` via `rume_data::sql`).
  - `WebhookDispatcher` integrating with `OutboxRelay` / `JobQueue` and exponential backoff retry scheduling, recording delivery attempts and HTTP status in `WebhookDeliveryStore`.
- **Inbound Webhooks (`rume-http::webhooks`)**:
  - `WebhookSignatureConfig` (`secret_resolver`, `max_age: Duration` default 300s, `max_body_bytes: usize` default 1 MiB, `header_name: "X-Rume-Signature"`).
  - `VerifyWebhookSignatureLayer` Axum middleware reading body bytes up to `max_body_bytes` (returning `413 Payload Too Large` on excess), validating `t=` timestamp freshness against both expired and future-skewed timestamps (`now.abs_diff(t) > max_age`, returning `401 Unauthorized`), hex-decoding `v1=` into `[u8; 32]` and verifying HMAC-SHA256 in constant time (`subtle::ConstantTimeEq`), and forwarding the intact body to the inner handler.
- **Transactional Email (`rume-messaging::mailer` & `rume-testing::mailer`)**:
  - `EmailMessage` (`to`, `from`, `subject`, `text_body`, `html_body`), `Mailer` trait (`send`, `send_template`).
  - `TemplateRenderer` using `minijinja` (under `mailer` feature) to render subject/text/HTML templates from context JSON.
  - `SmtpMailer` using `lettre::AsyncSmtpTransport` (under `mailer` feature).
  - `TestMailer` in `rume-testing::mailer` capturing sent `EmailMessage`s in-memory with `sent()`, `clear()`, `assert_sent_to(...)`.

## Constraints honoured

- `crates/messaging/src/outbox/relay.rs` (`OutboxRelay` dispatcher contract).
- `crates/data/src/sql.rs` (`rume_webhook_deliveries` SQL assets).
- `subtle::ConstantTimeEq` for signature verification (no `==` on MACs).
- `#![forbid(unsafe_code)]`, `rules/80-quality-bar.md`.

## Named tests (bodies omitted)

- `rume_messaging::webhooks::tests::outbound_webhook_dispatcher_signs_and_logs_delivery_with_backoff` — dispatches a webhook through `WebhookDispatcher` to a mock transport, asserts `X-Rume-Signature` format and `WebhookDeliveryStore` success/retry log entries.
- `rume_http::webhooks::tests::inbound_webhook_middleware_accepts_valid_signature_and_rejects_tampered_or_replayed` — sends valid, tampered-body, and expired-timestamp requests through `VerifyWebhookSignatureLayer`, asserting `200`, `401`, and `401`.
- `rume_messaging::mailer::tests::minijinja_template_renderer_renders_subject_and_body` — (under `mailer`) renders a `minijinja` template with variables and verifies output.
- `rume_testing::mailer::tests::test_mailer_captures_sent_emails` — sends emails via `TestMailer` as `dyn Mailer` and asserts captured recipients, subject, and body.

## Verification

```bash
cargo test -j 2 -p rume-messaging -p rume-http -p rume-testing --all-features
```
Expected: all `webhooks`, `mailer`, and `rume-testing` tests pass with zero warnings.

## Commit

Only when the user asks:
```text
feat(messaging,http,testing): add outbound/inbound HMAC webhooks and Mailer/SmtpMailer/TestMailer
```

## Done-when

- [ ] Outbound `WebhookDispatcher` signs `X-Rume-Signature`, retries with exponential backoff, and persists delivery logs.
- [ ] Inbound `VerifyWebhookSignatureLayer` validates HMAC-SHA256 in constant time and enforces timestamp replay protection.
- [ ] `Mailer`, feature-gated `SmtpMailer` (`lettre` + `minijinja`), and `TestMailer` are implemented and tested.
