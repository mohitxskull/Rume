-- Schema DDL for rume_webhook_deliveries in MySQL.
CREATE TABLE IF NOT EXISTS rume_webhook_deliveries (
    id              VARCHAR(128) NOT NULL PRIMARY KEY,
    endpoint_id     VARCHAR(255) NOT NULL,
    event_type      VARCHAR(255) NOT NULL,
    payload         JSON         NOT NULL,
    status          VARCHAR(32)  NOT NULL DEFAULT 'pending',
    attempt         INT          NOT NULL DEFAULT 0,
    next_attempt_at DATETIME(6),
    last_response   TEXT,
    created_at      DATETIME(6)  NOT NULL DEFAULT CURRENT_TIMESTAMP(6),
    updated_at      DATETIME(6)  NOT NULL DEFAULT CURRENT_TIMESTAMP(6) ON UPDATE CURRENT_TIMESTAMP(6),
    KEY rume_webhook_deliveries_status (status, next_attempt_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
