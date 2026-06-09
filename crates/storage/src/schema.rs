//! Diesel-схема (соответствует миграциям в `migrations/`). Денежные величины и amount
//! хранятся как TEXT (десятичная строка U256) — без bigdecimal, лосслессно (см. pg.rs).

diesel::table! {
    users (id) {
        id -> Uuid,
        email -> Text,
        password_hash -> Text,
        kyc_status -> Text,
        role -> Text,
        hd_account_index -> Int4,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    wallets (id) {
        id -> Uuid,
        user_id -> Uuid,
        chain -> Text,
        address -> Text,
        derivation_path -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    transactions (id) {
        id -> Uuid,
        wallet_id -> Uuid,
        chain -> Text,
        tx_hash -> Nullable<Text>,
        direction -> Text,
        to_address -> Nullable<Text>,
        amount_raw -> Text,
        fee_raw -> Nullable<Text>,
        status -> Text,
        idempotency_key -> Nullable<Text>,
        tracking -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    audit_log (id) {
        id -> Int8,
        actor -> Nullable<Uuid>,
        action -> Text,
        wallet_id -> Nullable<Uuid>,
        result -> Text,
        created_at -> Timestamptz,
    }
}
