-- VaultBridge schema. Денежные величины — TEXT (десятичная строка U256).
-- Запуск: `diesel migration run` (или применить вручную). DATABASE_URL → Postgres.

CREATE TABLE users (
    id                UUID PRIMARY KEY,
    email             TEXT NOT NULL UNIQUE,
    password_hash     TEXT NOT NULL,
    kyc_status        TEXT NOT NULL DEFAULT 'pending'
                      CHECK (kyc_status IN ('pending','approved','rejected')),
    role              TEXT NOT NULL DEFAULT 'user'
                      CHECK (role IN ('user','operator')),
    hd_account_index  INT  NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE wallets (
    id               UUID PRIMARY KEY,
    user_id          UUID NOT NULL REFERENCES users(id),
    chain            TEXT NOT NULL CHECK (chain IN ('ethereum','bitcoin','solana')),
    address          TEXT NOT NULL,
    derivation_path  TEXT NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (chain, address),
    UNIQUE (user_id, derivation_path)
);

CREATE TABLE transactions (
    id               UUID PRIMARY KEY,
    wallet_id        UUID NOT NULL REFERENCES wallets(id),
    chain            TEXT NOT NULL CHECK (chain IN ('ethereum','bitcoin','solana')),
    tx_hash          TEXT,
    direction        TEXT NOT NULL CHECK (direction IN ('incoming','outgoing')),
    to_address       TEXT,
    amount_raw       TEXT NOT NULL,
    fee_raw          TEXT,
    status           TEXT NOT NULL DEFAULT 'created'
                     CHECK (status IN ('created','signing','broadcast','unconfirmed','confirmed','failed','expired','replaced')),
    idempotency_key  TEXT,
    tracking         TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_tx_wallet ON transactions (wallet_id);
-- Отдельным шагом, чтобы миграция оставалась идемпотентной и на уже существующих БД
-- (CREATE TABLE IF NOT EXISTS новую колонку не добавит).
ALTER TABLE transactions ADD COLUMN IF NOT EXISTS tracking TEXT;

-- Append-only аудит. Для тамперо-устойчивости в проде:
--   REVOKE UPDATE, DELETE ON audit_log FROM app_role;
CREATE TABLE audit_log (
    id          BIGSERIAL PRIMARY KEY,
    actor       UUID,
    action      TEXT NOT NULL,
    wallet_id   UUID,
    result      TEXT NOT NULL CHECK (result IN ('ok','denied','error')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
