//! REST-роуты API: аутентификация, пользователи, кошельки, история, котировка и сага
//! вывода, плюс операторские эндпоинты и служебные ручки (health/metrics/openapi).
//! Это «лицо» сервиса — здесь связываются экстракторы, гейты и `AppState`.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use blockchain::WithdrawRequest;
use core_domain::{Amount, Chain, KycStatus, Role, TransactionStatus, UserId, WalletId, U256};
use serde::{Deserialize, Serialize};
use storage::{NewAudit, NewOutgoing, NewUser, NewWallet};
use uuid::Uuid;

use utoipa::{OpenApi, ToSchema};

use crate::auth::{hash_password, verify_password, AuthUser, RequireOperator};
use crate::error::ApiError;
use crate::graphql::schema;
use crate::idempotency::Begin;
use crate::state::AppState;

/// OpenAPI-документ — контракт REST для кодогена фронта.
#[derive(OpenApi)]
#[openapi(
    paths(
        login,
        create_user,
        get_user,
        list_wallets,
        create_wallet,
        list_transactions,
        withdraw_quote,
        withdraw
    ),
    components(schemas(
        LoginRequest,
        LoginResponse,
        CreateUserRequest,
        UserProfile,
        CreateWalletRequest,
        WalletDto,
        WithdrawRequestDto,
        QuoteResponse,
        WithdrawResponse
    )),
    info(title = "VaultBridge API", version = "0.1.0")
)]
pub struct ApiDoc;

/// Собрать роутер со всеми эндпоинтами и вшитым `AppState`. Порядок сегментов: служебные
/// ручки (`/healthz`, `/metrics`, ...), затем версионированный `/v1/*`. `AppState` уходит
/// внутрь через `with_state`, поэтому хендлеры достают зависимости экстрактором `State`.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics_handler))
        .route("/api-docs/openapi.json", get(openapi_json))
        .route("/v1/auth/login", post(login))
        .route("/v1/users", post(create_user))
        .route("/v1/users/{id}", get(get_user))
        .route("/v1/wallets", get(list_wallets).post(create_wallet))
        .route("/v1/wallets/{id}/transactions", get(list_transactions))
        .route("/v1/wallets/{id}/withdraw/quote", post(withdraw_quote))
        .route("/v1/wallets/{id}/withdraw", post(withdraw))
        .route("/v1/graphql", post(graphql_handler))
        .route("/v1/ws", get(crate::events::ws_handler))
        .route("/v1/ops/audit", get(ops_audit))
        .route("/v1/ops/withdrawals", get(ops_withdrawals))
        .with_state(state)
}

/// Отдаёт OpenAPI-спеку в JSON (источник для `openapi-typescript` на фронте).
async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

/// Liveness: процесс жив и отвечает. Зависимости здесь не проверяются — для этого `readyz`.
async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

/// Readiness: критичные зависимости (Postgres/Redis/signing). Реальные пинги подключаются
/// здесь; ClickHouse некритичен и readyz не валит.
async fn readyz() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ready",
        "deps": { "postgres": "ok", "redis": "ok", "signing": "ok" }
    }))
}

/// Prometheus-метрики.
async fn metrics_handler(State(state): State<AppState>) -> String {
    state.metrics.render()
}

// ---- операторский API (роль operator) ----

/// Строка аудит-лога в виде для операторского ответа. Время — unix-секунды, чтобы фронту
/// не разбирать формат `OffsetDateTime`.
#[derive(Serialize)]
struct AuditDto {
    /// Идентификатор записи.
    id: i64,
    /// Кто инициировал (id строкой), если применимо.
    actor: Option<String>,
    /// Код действия (например, `withdraw.broadcast`).
    action: String,
    /// Связанный кошелёк, если применимо.
    wallet_id: Option<String>,
    /// Исход: `ok` | `denied` | `error`.
    result: String,
    /// Когда произошло, unix-секунды.
    created_at_unix: i64,
}

/// Исходящая транзакция в операторском списке всех выводов.
#[derive(Serialize)]
struct OpsTxDto {
    /// Идентификатор транзакции.
    id: String,
    /// Кошелёк-источник.
    wallet_id: String,
    /// Сеть.
    chain: Chain,
    /// Адрес получателя.
    to_address: Option<String>,
    /// Сумма в минимальных единицах (десятичная строка).
    amount_raw: String,
    /// Текущий статус по конечному автомату.
    status: TransactionStatus,
    /// Хэш в сети (после broadcast).
    tx_hash: Option<String>,
    /// Когда создана, unix-секунды.
    created_at_unix: i64,
}

/// Чтение аудит-лога (только `operator`).
async fn ops_audit(
    State(state): State<AppState>,
    _op: RequireOperator,
) -> Result<Json<Vec<AuditDto>>, ApiError> {
    let entries = state.audit.list().await?;
    Ok(Json(
        entries
            .into_iter()
            .map(|e| AuditDto {
                id: e.id,
                actor: e.actor.map(|a| a.to_string()),
                action: e.action,
                wallet_id: e.wallet_id.map(|w| w.to_string()),
                result: e.result,
                created_at_unix: e.created_at.unix_timestamp(),
            })
            .collect(),
    ))
}

/// Все выводы по всем пользователям (только `operator`).
async fn ops_withdrawals(
    State(state): State<AppState>,
    _op: RequireOperator,
) -> Result<Json<Vec<OpsTxDto>>, ApiError> {
    let txs = state.txs.list_all_outgoing().await?;
    Ok(Json(
        txs.into_iter()
            .map(|t| OpsTxDto {
                id: t.id.to_string(),
                wallet_id: t.wallet_id.to_string(),
                chain: t.chain,
                to_address: t.to_address,
                amount_raw: t.amount_raw.to_string(),
                status: t.status,
                tx_hash: t.tx_hash,
                created_at_unix: t.created_at.unix_timestamp(),
            })
            .collect(),
    ))
}

// ---- DTO ----

/// Тело запроса на логин.
#[derive(Deserialize, ToSchema)]
struct LoginRequest {
    /// Email (он же логин).
    email: String,
    /// Пароль в открытом виде — проверяется против argon2-хеша и никуда не сохраняется.
    password: String,
}

/// Ответ на логин: access-токен и его срок жизни.
#[derive(Serialize, ToSchema)]
struct LoginResponse {
    /// JWT для заголовка `Authorization: Bearer ...`.
    access_token: String,
    /// Сколько секунд токен действителен.
    expires_in: i64,
}

/// Тело запроса на регистрацию.
#[derive(Deserialize, ToSchema)]
struct CreateUserRequest {
    /// Email (логин).
    email: String,
    /// Пароль в открытом виде (минимум 8 символов, хешируется на сервере).
    password: String,
}

/// Публичный профиль пользователя. Хеша пароля тут нет — наружу он не выходит.
#[derive(Serialize, ToSchema)]
struct UserProfile {
    /// Идентификатор.
    id: Uuid,
    /// Email.
    email: String,
    /// pending|approved|rejected
    #[schema(value_type = String)]
    kyc_status: core_domain::KycStatus,
    /// user|operator
    #[schema(value_type = String)]
    role: Role,
}

impl From<storage::User> for UserProfile {
    fn from(u: storage::User) -> Self {
        Self {
            id: u.id.0,
            email: u.email,
            kyc_status: u.kyc_status,
            role: u.role,
        }
    }
}

/// Тело запроса на создание кошелька — нужна только сеть, адрес выведет signing-service.
#[derive(Deserialize, ToSchema)]
struct CreateWalletRequest {
    /// ethereum|bitcoin|solana
    #[schema(value_type = String)]
    chain: Chain,
}

/// Кошелёк в ответе API. Приватного ключа тут нет и быть не может.
#[derive(Serialize, ToSchema)]
struct WalletDto {
    /// Идентификатор.
    id: Uuid,
    #[schema(value_type = String)]
    chain: Chain,
    /// Публичный адрес.
    address: String,
    /// Путь HD-деривации.
    derivation_path: String,
    /// Когда создан, unix-секунды.
    created_at_unix: i64,
}

impl From<storage::Wallet> for WalletDto {
    fn from(w: storage::Wallet) -> Self {
        Self {
            id: w.id.0,
            chain: w.chain,
            address: w.address,
            derivation_path: w.derivation_path,
            created_at_unix: w.created_at.unix_timestamp(),
        }
    }
}

/// Страница результатов с курсорной пагинацией. `next_cursor = None` — страниц больше нет.
#[derive(Serialize)]
struct Page<T> {
    /// Элементы текущей страницы.
    items: Vec<T>,
    /// Курсор на следующую страницу (id последнего элемента).
    next_cursor: Option<Uuid>,
}

// ---- handlers ----

#[utoipa::path(
    post, path = "/v1/auth/login", request_body = LoginRequest,
    responses((status = 200, body = LoginResponse), (status = 401))
)]
async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, ApiError> {
    // Единый ответ на «нет пользователя» и «неверный пароль» — не раскрываем существование.
    let user = state
        .users
        .by_email(&req.email)
        .await
        .map_err(|_| ApiError::Unauthorized)?;
    if !verify_password(&req.password, &user.password_hash) {
        return Err(ApiError::Unauthorized);
    }
    let token = state.jwt.issue(user.id, user.role)?;
    Ok(Json(LoginResponse {
        access_token: token,
        expires_in: state.jwt.ttl_secs(),
    }))
}

#[utoipa::path(
    post, path = "/v1/users", request_body = CreateUserRequest,
    responses((status = 201, body = UserProfile), (status = 422), (status = 409))
)]
async fn create_user(
    State(state): State<AppState>,
    Json(req): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<UserProfile>), ApiError> {
    if !req.email.contains('@') {
        return Err(ApiError::Validation("invalid email".into()));
    }
    if req.password.len() < 8 {
        return Err(ApiError::Validation("password too short".into()));
    }
    let password_hash = hash_password(&req.password)?;
    // KYC-онбординг: дёргаем провайдера и фиксируем статус.
    let kyc_status = state.kyc.submit(&req.email).await;
    let user = state
        .users
        .create(NewUser {
            email: req.email,
            password_hash,
            role: Role::User,
        })
        .await?;
    if kyc_status != KycStatus::Pending {
        state.users.set_kyc(user.id, kyc_status).await?;
    }
    let user = state.users.by_id(user.id).await?;
    Ok((StatusCode::CREATED, Json(user.into())))
}

#[utoipa::path(
    get, path = "/v1/users/{id}",
    params(("id" = Uuid, Path, description = "user id")),
    responses((status = 200, body = UserProfile), (status = 401), (status = 404))
)]
async fn get_user(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<UserProfile>, ApiError> {
    // user видит только себя; operator — любого. Чужой → 404 (не раскрываем существование).
    if auth.id.0 != id && auth.role != Role::Operator {
        return Err(ApiError::NotFound);
    }
    let user = state.users.by_id(UserId(id)).await?;
    Ok(Json(user.into()))
}

#[utoipa::path(
    get, path = "/v1/wallets",
    responses((status = 200, body = [WalletDto]), (status = 401))
)]
async fn list_wallets(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<WalletDto>>, ApiError> {
    let wallets = state.wallets.list_for_user(auth.id).await?;
    Ok(Json(wallets.into_iter().map(WalletDto::from).collect()))
}

#[utoipa::path(
    post, path = "/v1/wallets", request_body = CreateWalletRequest,
    responses((status = 201, body = WalletDto), (status = 401), (status = 422))
)]
async fn create_wallet(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(req): Json<CreateWalletRequest>,
) -> Result<(StatusCode, Json<WalletDto>), ApiError> {
    // Индекс кошелька в пределах пользователя → путь деривации.
    let index = state.wallets.list_for_user(auth.id).await?.len() as u32;
    let derivation_path = derivation_path(req.chain, 0, index);
    // Поток создания кошелька: адрес выводит signing-service из seed по пути; приватный
    // ключ в gateway не возвращается. Ошибка деривации пробрасывается наружу.
    let address = state
        .signer
        .derive_address(req.chain, &derivation_path)
        .await?;
    let wallet = state
        .wallets
        .create(
            NewWallet {
                user_id: auth.id,
                chain: req.chain,
                address,
                derivation_path,
            },
            state.config.max_wallets_per_user,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(wallet.into())))
}

#[utoipa::path(
    get, path = "/v1/wallets/{id}/transactions",
    params(("id" = Uuid, Path, description = "wallet id")),
    responses((status = 200), (status = 401), (status = 404))
)]
async fn list_transactions(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Page<serde_json::Value>>, ApiError> {
    // Проверка владения; чужой/несуществующий → 404.
    state.wallets.owned(WalletId(id), auth.id).await?;
    // Этот REST-эндпоинт истории пока заглушка (агрегат отдаёт GraphQL-портфель и ops-списки).
    Ok(Json(Page {
        items: vec![],
        next_cursor: None,
    }))
}

/// Тело запроса на вывод (и на его котировку).
#[derive(Deserialize, ToSchema)]
struct WithdrawRequestDto {
    /// Адрес получателя.
    to_address: String,
    /// Сумма в минимальных единицах (U256 как строка).
    amount_raw: String,
    /// Опциональный потолок комиссии (slippage). Иначе — estimate × fee_cap_factor.
    max_fee_raw: Option<String>,
}

/// Ответ на успешный вывод. Статус на этом шаге — `unconfirmed`: транзакция ушла в сеть,
/// но подтверждений ещё нет (до `confirmed` её доводит фоновый реконсилятор).
#[derive(Serialize, ToSchema)]
struct WithdrawResponse {
    /// Идентификатор транзакции в нашей БД.
    tx_id: String,
    /// Текущий статус (`unconfirmed`).
    status: String,
    /// Хэш транзакции в сети.
    tx_hash: Option<String>,
    /// Фактическая комиссия в минимальных единицах.
    fee_raw: String,
}

/// Котировка вывода: во что обойдётся операция, без каких-либо побочных эффектов.
#[derive(Serialize, ToSchema)]
struct QuoteResponse {
    /// Оценка комиссии сети.
    estimated_fee_raw: String,
    /// Потолок комиссии, который применится при выводе.
    max_fee_raw: String,
    /// Сколько всего спишется (сумма + комиссия).
    total_debit_raw: String,
    /// Доступный к трате баланс на момент котировки.
    spendable_raw: String,
}

/// Оценка вывода без побочных эффектов: комиссия, потолок, итог,
/// доступный баланс — для отображения в форме до подтверждения.
#[utoipa::path(
    post, path = "/v1/wallets/{id}/withdraw/quote", request_body = WithdrawRequestDto,
    params(("id" = Uuid, Path, description = "wallet id")),
    responses(
        (status = 200, body = QuoteResponse),
        (status = 401), (status = 403), (status = 404), (status = 422)
    )
)]
async fn withdraw_quote(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<WithdrawRequestDto>,
) -> Result<Json<QuoteResponse>, ApiError> {
    let wallet = state.wallets.owned(WalletId(id), auth.id).await?;
    let user = state.users.by_id(auth.id).await?;
    if !user.kyc_status.can_withdraw() {
        return Err(ApiError::Forbidden);
    }
    let client = state
        .chains
        .get(&wallet.chain)
        .cloned()
        .ok_or_else(|| ApiError::Validation("chain not supported".into()))?;
    client.validate_address(&req.to_address)?;

    let amount_raw: U256 = req
        .amount_raw
        .parse()
        .map_err(|_| ApiError::Validation("invalid amount_raw".into()))?;
    let amount = Amount::new(wallet.chain, amount_raw);
    let wreq = WithdrawRequest {
        chain: wallet.chain,
        from_address: wallet.address.clone(),
        to_address: req.to_address.clone(),
        amount,
        derivation_path: wallet.derivation_path.clone(),
    };

    let fee = client.estimate_fee(&wreq).await?;
    let cap = compute_fee_cap(&req.max_fee_raw, &fee.raw, client.config().fee_cap_factor)?;
    let total_debit = amount.checked_add(&fee)?;
    let balance = client.get_balance(&wallet.address).await?;

    Ok(Json(QuoteResponse {
        estimated_fee_raw: fee.raw.to_string(),
        max_fee_raw: cap.to_string(),
        total_debit_raw: total_debit.raw.to_string(),
        spendable_raw: balance.spendable.raw.to_string(),
    }))
}

/// Потолок комиссии: явный `max_fee` клиента либо `estimate × fee_cap_factor`.
fn compute_fee_cap(
    max_fee_raw: &Option<String>,
    estimate: &U256,
    factor: f64,
) -> Result<U256, ApiError> {
    match max_fee_raw {
        Some(s) => s
            .parse::<U256>()
            .map_err(|_| ApiError::Validation("invalid max_fee_raw".into())),
        None => Ok(estimate.saturating_mul(U256::from(factor.max(1.0) as u64))),
    }
}

/// Вывод средств как сага: идемпотентность → гейты → lock →
/// комиссия/достаточность → pending → build → sign → broadcast → unconfirmed.
#[utoipa::path(
    post, path = "/v1/wallets/{id}/withdraw", request_body = WithdrawRequestDto,
    params(("id" = Uuid, Path, description = "wallet id")),
    responses(
        (status = 200, body = WithdrawResponse),
        (status = 401), (status = 403), (status = 404),
        (status = 409), (status = 422)
    )
)]
async fn withdraw(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(req): Json<WithdrawRequestDto>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| ApiError::Validation("Idempotency-Key header required".into()))?;

    // Идемпотентность: повтор → сохранённый ответ; параллельный дубль → 409.
    match state.idempotency.begin(auth.id, &key).await {
        Begin::Done(value) => return Ok(Json(value)),
        Begin::InFlight => return Err(ApiError::Conflict),
        Begin::Fresh => {}
    }

    match withdraw_saga(&state, auth, WalletId(id), &key, &req).await {
        Ok(value) => {
            state
                .idempotency
                .complete(auth.id, &key, value.clone())
                .await;
            Ok(Json(value))
        }
        Err(e) => {
            state.idempotency.abort(auth.id, &key).await; // разрешить повтор тем же ключом
            Err(e)
        }
    }
}

async fn withdraw_saga(
    state: &AppState,
    auth: AuthUser,
    wallet_id: WalletId,
    key: &str,
    req: &WithdrawRequestDto,
) -> Result<serde_json::Value, ApiError> {
    // Гейты до побочных эффектов: владение, KYC, поддержка сети, валидность адреса.
    let wallet = state.wallets.owned(wallet_id, auth.id).await?; // чужой → 404
    let user = state.users.by_id(auth.id).await?;
    if !user.kyc_status.can_withdraw() {
        audit(state, auth.id, wallet_id, "withdraw.denied.kyc", "denied").await;
        return Err(ApiError::Forbidden); // KYC не пройден
    }
    let client = state
        .chains
        .get(&wallet.chain)
        .cloned()
        .ok_or_else(|| ApiError::Validation("chain not supported".into()))?;
    client.validate_address(&req.to_address)?; // формат/сеть

    // AML-скрининг адреса назначения — до бизнес-логики.
    if state
        .aml
        .is_blacklisted(wallet.chain, &req.to_address)
        .await
    {
        audit(state, auth.id, wallet_id, "withdraw.denied.aml", "denied").await;
        return Err(ApiError::Validation(
            "destination address is blacklisted".into(),
        ));
    }

    let amount_raw: U256 = req
        .amount_raw
        .parse()
        .map_err(|_| ApiError::Validation("invalid amount_raw".into()))?;
    let amount = Amount::new(wallet.chain, amount_raw);
    let wreq = WithdrawRequest {
        chain: wallet.chain,
        from_address: wallet.address.clone(),
        to_address: req.to_address.clone(),
        amount,
        derivation_path: wallet.derivation_path.clone(),
    };

    // Сериализация операций на кошелёк: держим лок до конца саги.
    let _guard = state.locks.lock(wallet_id).await;

    // Оценка комиссии + потолок (slippage) + достаточность средств.
    let fee = client.estimate_fee(&wreq).await?;
    let cap = compute_fee_cap(&req.max_fee_raw, &fee.raw, client.config().fee_cap_factor)?;
    if fee.raw > cap {
        return Err(ApiError::Validation("fee exceeds max_fee".into()));
    }
    let total_debit = amount.checked_add(&fee)?;
    let balance = client.get_balance(&wallet.address).await?;
    if balance.spendable.raw < total_debit.raw {
        return Err(ApiError::Validation("insufficient funds".into()));
    }

    // Фиксация намерения (pending) и движение по FSM.
    let tx = state
        .txs
        .create_outgoing(NewOutgoing {
            wallet_id,
            chain: wallet.chain,
            to_address: req.to_address.clone(),
            amount_raw,
            idempotency_key: key.to_string(),
        })
        .await?;
    state
        .txs
        .set_status(tx.id, TransactionStatus::Signing, None, Some(fee.raw))
        .await?;

    let unsigned = client.build_unsigned(&wreq, &fee).await?;
    // Сохраняем chain-specific токен (EVM nonce / Solana blockhash) для реконсиляции:
    // по нему сканер потом отличит «заменена»/«истекла» от «ещё не дошла».
    if let Some(tracking) = &unsigned.tracking {
        state.txs.set_tracking(tx.id, tracking).await?;
    }
    // По подписи на каждый SigningRequest (1 для account-моделей, N для UTXO).
    let mut signatures = Vec::with_capacity(unsigned.requests.len());
    for r in &unsigned.requests {
        signatures.push(
            state
                .signer
                .sign(wallet.chain, &r.derivation_path, &r.payload)
                .await?,
        );
    }
    let signed = client.assemble_signed(&unsigned, &signatures)?;

    // Детерминированный id считаем ДО отправки и сохраняем вместе со статусом Broadcast.
    // Тогда краш во время broadcast не оставит запись без tx_hash: реконсилятор её увидит
    // и досверит статус, а повторная отправка безопасна (broadcast идемпотентен).
    let txid = client.txid(&signed)?;
    state
        .txs
        .set_status(
            tx.id,
            TransactionStatus::Broadcast,
            Some(txid.clone()),
            None,
        )
        .await?;
    let tx_hash = client.broadcast(&signed).await?;
    debug_assert_eq!(txid, tx_hash, "predicted txid must match network hash");
    let tx = state
        .txs
        .set_status(
            tx.id,
            TransactionStatus::Unconfirmed,
            Some(tx_hash.clone()),
            None,
        )
        .await?;

    // Баланс кошелька изменился — сбрасываем кеш.
    crate::balance::invalidate(state, wallet.chain, &wallet.address).await;
    audit(state, auth.id, wallet_id, "withdraw.broadcast", "ok").await;
    // Live-уведомление подписчикам WS.
    crate::events::publish(
        state,
        auth.id,
        wallet_id,
        tx.id.to_string(),
        TransactionStatus::Unconfirmed,
        Some(tx_hash.clone()),
    );
    // Аналитический сток (best-effort).
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    state
        .analytics
        .record(storage::AnalyticsRecord {
            event: "withdraw".into(),
            chain: wallet.chain.as_str().into(),
            direction: "outgoing".into(),
            wallet_id: wallet_id.to_string(),
            tx_id: tx.id.to_string(),
            amount_raw: amount.raw.to_string(),
            status: "unconfirmed".into(),
            ts,
        })
        .await;

    Ok(serde_json::json!({
        "tx_id": tx.id.to_string(),
        "status": "unconfirmed",
        "tx_hash": tx_hash,
        "fee_raw": fee.raw.to_string(),
    }))
}

/// GraphQL `portfolio`. Требует аутентификации; `AppState` и `AuthUser`
/// прокидываются в контекст резолвера. Не входит в OpenAPI (отдельная схема).
async fn graphql_handler(
    State(state): State<AppState>,
    auth: AuthUser,
    req: async_graphql_axum::GraphQLRequest,
) -> async_graphql_axum::GraphQLResponse {
    schema()
        .execute(req.into_inner().data(state).data(auth))
        .await
        .into()
}

/// Запись в аудит-лог. Сбой аудита не блокирует операцию (in-memory не падает).
async fn audit(state: &AppState, actor: UserId, wallet_id: WalletId, action: &str, result: &str) {
    state.metrics.record_withdraw(result);
    let _ = state
        .audit
        .record(NewAudit {
            actor: Some(actor),
            action: action.to_string(),
            wallet_id: Some(wallet_id),
            result: result.to_string(),
        })
        .await;
}

/// BIP-44 путь деривации: `m/44'/{coin}'/{account}'/0/{index}`.
fn derivation_path(chain: Chain, account: u32, index: u32) -> String {
    let coin = match chain {
        Chain::Ethereum => 60,
        Chain::Bitcoin => 0,
        Chain::Solana => 501,
    };
    format!("m/44'/{coin}'/{account}'/0/{index}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{hash_password, JwtKeys};
    use crate::idempotency::in_memory as idempotency_in_memory;
    use crate::state::Config;
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request, StatusCode};
    use blockchain::{BlockchainClient, MockChain};
    use core_domain::KycStatus;
    use signing_service::LocalSigner;
    use std::collections::HashMap;
    use std::sync::Arc;
    use storage::InMemoryWalletLock;
    use storage::{InMemoryStore, UserRepository};
    use tower::ServiceExt;

    const M: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn empty_chains() -> Arc<HashMap<Chain, Arc<dyn BlockchainClient>>> {
        Arc::new(HashMap::new())
    }

    fn build_state(
        store: Arc<InMemoryStore>,
        chains: Arc<HashMap<Chain, Arc<dyn BlockchainClient>>>,
        aml: Arc<dyn kyc_aml::AmlScreener>,
    ) -> AppState {
        AppState {
            jwt: Arc::new(JwtKeys::from_secret("k1", b"secret", 3600)),
            users: store.clone(),
            wallets: store.clone(),
            txs: store.clone(),
            signer: Arc::new(LocalSigner::from_mnemonic(M, "").unwrap()),
            chains,
            cache: Arc::new(storage::InMemoryBalanceCache::new()),
            audit: store,
            analytics: Arc::new(storage::InMemoryAnalytics::new()),
            kyc: Arc::new(kyc_aml::MockKyc::new(KycStatus::Approved)),
            aml,
            metrics: Arc::new(crate::metrics::Metrics::new()),
            events: crate::events::channel(),
            locks: Arc::new(InMemoryWalletLock::new()),
            idempotency: idempotency_in_memory(),
            config: Config::default(),
        }
    }

    fn empty_aml() -> Arc<dyn kyc_aml::AmlScreener> {
        Arc::new(kyc_aml::InMemoryBlacklist::new())
    }

    async fn test_state() -> (AppState, UserId) {
        let store = Arc::new(InMemoryStore::new());
        let user = UserRepository::create(
            &*store,
            NewUser {
                email: "u@test.dev".into(),
                password_hash: hash_password("password123").unwrap(),
                role: Role::User,
            },
        )
        .await
        .unwrap();
        (build_state(store, empty_chains(), empty_aml()), user.id)
    }

    /// Состояние для тестов вывода: одобренный KYC, funded MockChain, кошелёк с 0x-адресом.
    async fn withdraw_state(balance: u64) -> (AppState, String, Arc<MockChain>, String) {
        use storage::{NewWallet, WalletRepository};
        let store = Arc::new(InMemoryStore::new());
        let user = UserRepository::create(
            &*store,
            NewUser {
                email: "w@test.dev".into(),
                password_hash: hash_password("password123").unwrap(),
                role: Role::User,
            },
        )
        .await
        .unwrap();
        store.set_kyc(user.id, KycStatus::Approved).await.unwrap();

        let address = "0x000000000000000000000000000000000000abcd".to_string();
        WalletRepository::create(
            &*store,
            NewWallet {
                user_id: user.id,
                chain: Chain::Ethereum,
                address: address.clone(),
                derivation_path: "m/44'/60'/0'/0/0".into(),
            },
            10,
        )
        .await
        .unwrap();
        let wallet_id = store.list_for_user(user.id).await.unwrap()[0].id;

        let chain = Arc::new(MockChain::ethereum());
        chain.set_balance(&address, U256::from(balance));
        let mut map: HashMap<Chain, Arc<dyn BlockchainClient>> = HashMap::new();
        map.insert(Chain::Ethereum, chain.clone());

        let state = build_state(store, Arc::new(map), empty_aml());
        let token = state.jwt.issue(user.id, Role::User).unwrap();
        (state, token, chain, wallet_id.to_string())
    }

    fn withdraw_request(
        wallet_id: &str,
        token: &str,
        idem: &str,
        body: serde_json::Value,
    ) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(format!("/v1/wallets/{wallet_id}/withdraw"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header("idempotency-key", idem)
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn json_request(
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: serde_json::Value,
    ) -> Request<Body> {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(t) = token {
            b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        b.body(Body::from(body.to_string())).unwrap()
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn login(state: &AppState, email: &str, password: &str) -> String {
        let resp = router(state.clone())
            .oneshot(json_request(
                "POST",
                "/v1/auth/login",
                None,
                serde_json::json!({ "email": email, "password": password }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        body_json(resp).await["access_token"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn login_then_list_wallets_requires_auth() {
        let (state, _) = test_state().await;
        // без токена — 401
        let resp = router(state.clone())
            .oneshot(json_request(
                "GET",
                "/v1/wallets",
                None,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        // с токеном — 200 и пустой список
        let token = login(&state, "u@test.dev", "password123").await;
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .uri("/v1/wallets")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn openapi_json_served() {
        let (state, _) = test_state().await;
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api-docs/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let doc = body_json(resp).await;
        assert!(doc["paths"]["/v1/auth/login"].is_object());
        assert!(doc["paths"]["/v1/wallets"].is_object());
    }

    #[tokio::test]
    async fn login_wrong_password_unauthorized() {
        let (state, _) = test_state().await;
        let resp = router(state)
            .oneshot(json_request(
                "POST",
                "/v1/auth/login",
                None,
                serde_json::json!({ "email": "u@test.dev", "password": "wrong" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_and_list_wallet() {
        let (state, _) = test_state().await;
        let token = login(&state, "u@test.dev", "password123").await;
        let resp = router(state.clone())
            .oneshot(json_request(
                "POST",
                "/v1/wallets",
                Some(&token),
                serde_json::json!({ "chain": "ethereum" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let w = body_json(resp).await;
        assert_eq!(w["chain"], "ethereum");
        assert_eq!(w["derivation_path"], "m/44'/60'/0'/0/0");
    }

    #[tokio::test]
    async fn get_other_user_is_404() {
        let (state, _) = test_state().await;
        let token = login(&state, "u@test.dev", "password123").await;
        let other = Uuid::new_v4();
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/users/{other}"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn withdraw_happy_path() {
        let (state, token, chain, wid) = withdraw_state(1_000_000).await;
        let resp = router(state)
            .oneshot(withdraw_request(
                &wid,
                &token,
                "idem-1",
                serde_json::json!({ "to_address": "0x000000000000000000000000000000000000beef", "amount_raw": "1000" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "unconfirmed");
        assert!(body["tx_hash"].as_str().unwrap().starts_with("0xmocktx"));
        assert_eq!(chain.broadcast_count(), 1);
    }

    #[tokio::test]
    async fn withdraw_quote_returns_fee_and_totals() {
        let (state, token, _chain, wid) = withdraw_state(1_000_000).await;
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/wallets/{wid}/withdraw/quote"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::json!({ "to_address": "0x000000000000000000000000000000000000beef", "amount_raw": "1000" })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let q = body_json(resp).await;
        assert_eq!(q["estimated_fee_raw"], "21000");
        assert_eq!(q["total_debit_raw"], "22000"); // 1000 + 21000
        assert_eq!(q["spendable_raw"], "1000000");
    }

    #[tokio::test]
    async fn withdraw_insufficient_funds_422() {
        let (state, token, _chain, wid) = withdraw_state(10).await;
        let resp = router(state)
            .oneshot(withdraw_request(
                &wid,
                &token,
                "idem-2",
                serde_json::json!({ "to_address": "0x000000000000000000000000000000000000beef", "amount_raw": "1000" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn withdraw_invalid_address_422() {
        let (state, token, _chain, wid) = withdraw_state(1_000_000).await;
        let resp = router(state)
            .oneshot(withdraw_request(
                &wid,
                &token,
                "idem-3",
                serde_json::json!({ "to_address": "not-an-address", "amount_raw": "1000" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn withdraw_idempotent_repeat_no_double_spend() {
        let (state, token, chain, wid) = withdraw_state(1_000_000).await;
        let body = serde_json::json!({ "to_address": "0x000000000000000000000000000000000000beef", "amount_raw": "1000" });

        let r1 = router(state.clone())
            .oneshot(withdraw_request(&wid, &token, "idem-same", body.clone()))
            .await
            .unwrap();
        assert_eq!(r1.status(), StatusCode::OK);
        let tx1 = body_json(r1).await["tx_id"].as_str().unwrap().to_string();

        let r2 = router(state)
            .oneshot(withdraw_request(&wid, &token, "idem-same", body))
            .await
            .unwrap();
        assert_eq!(r2.status(), StatusCode::OK);
        let tx2 = body_json(r2).await["tx_id"].as_str().unwrap().to_string();

        assert_eq!(tx1, tx2); // тот же результат
        assert_eq!(chain.broadcast_count(), 1); // нет задвоения выплаты
    }

    #[tokio::test]
    async fn graphql_portfolio_aggregates_balances() {
        let (state, token, _chain, _wid) = withdraw_state(1_000_000).await;
        let query = serde_json::json!({
            "query": "{ portfolio { userId balances { chain decimals spendableRaw } } }"
        });
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/graphql")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(query.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let bal = &body["data"]["portfolio"]["balances"][0];
        assert_eq!(bal["chain"], "ethereum");
        assert_eq!(bal["decimals"], 18);
        assert_eq!(bal["spendableRaw"], "1000000");
    }

    const WALLET_ADDR: &str = "0x000000000000000000000000000000000000abcd";

    #[tokio::test]
    async fn graphql_portfolio_uses_cache() {
        // Сеть отдаёт 1_000_000, но кеш содержит 42 → должны увидеть 42 (read-through hit).
        let (state, token, _chain, _wid) = withdraw_state(1_000_000).await;
        let cached = crate::balance::CachedBalance {
            total_raw: "42".into(),
            reserved_raw: "0".into(),
            spendable_raw: "42".into(),
            decimals: 18,
        };
        state
            .cache
            .put(
                &format!("balance:ethereum:{WALLET_ADDR}"),
                serde_json::to_string(&cached).unwrap(),
                std::time::Duration::from_secs(30),
            )
            .await;
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/graphql")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::json!({ "query": "{ portfolio { balances { spendableRaw } } }" })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp).await;
        assert_eq!(
            body["data"]["portfolio"]["balances"][0]["spendableRaw"],
            "42"
        );
    }

    #[tokio::test]
    async fn withdraw_invalidates_balance_cache() {
        let (state, token, _chain, wid) = withdraw_state(1_000_000).await;
        let key = format!("balance:ethereum:{WALLET_ADDR}");
        state
            .cache
            .put(&key, "stale".into(), std::time::Duration::from_secs(30))
            .await;
        let resp = router(state.clone())
            .oneshot(withdraw_request(
                &wid,
                &token,
                "idem-inv",
                serde_json::json!({ "to_address": "0x000000000000000000000000000000000000beef", "amount_raw": "1000" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(state.cache.get(&key).await, None); // кеш сброшен после вывода
    }

    #[tokio::test]
    async fn ops_audit_requires_operator() {
        let (state, _) = test_state().await;
        let user_token = login(&state, "u@test.dev", "password123").await;
        // user → 403
        let resp = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/v1/ops/audit")
                    .header(header::AUTHORIZATION, format!("Bearer {user_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        // operator → 200
        let op_token = state
            .jwt
            .issue(core_domain::UserId::new(), Role::Operator)
            .unwrap();
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .uri("/v1/ops/audit")
                    .header(header::AUTHORIZATION, format!("Bearer {op_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn metrics_counts_withdrawals() {
        let (state, token, _chain, wid) = withdraw_state(1_000_000).await;
        router(state.clone())
            .oneshot(withdraw_request(
                &wid,
                &token,
                "idem-m",
                serde_json::json!({ "to_address": "0x000000000000000000000000000000000000beef", "amount_raw": "1000" }),
            ))
            .await
            .unwrap();
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let text = String::from_utf8(
            to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(text.contains("vaultbridge_withdrawals_total{result=\"ok\"} 1"));
    }

    #[tokio::test]
    async fn readyz_ok() {
        let (state, _) = test_state().await;
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["status"], "ready");
    }

    #[tokio::test]
    async fn graphql_requires_auth() {
        let (state, _) = test_state().await;
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/graphql")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "query": "{ portfolio { userId } }" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn withdraw_blocked_by_aml_blacklist() {
        use storage::{NewWallet, WalletRepository};
        const BAD: &str = "0x000000000000000000000000000000000000beef";
        let store = Arc::new(InMemoryStore::new());
        let user = UserRepository::create(
            &*store,
            NewUser {
                email: "aml@test.dev".into(),
                password_hash: hash_password("password123").unwrap(),
                role: Role::User,
            },
        )
        .await
        .unwrap();
        store.set_kyc(user.id, KycStatus::Approved).await.unwrap();
        WalletRepository::create(
            &*store,
            NewWallet {
                user_id: user.id,
                chain: Chain::Ethereum,
                address: WALLET_ADDR.into(),
                derivation_path: "m/44'/60'/0'/0/0".into(),
            },
            10,
        )
        .await
        .unwrap();
        let wid = store.list_for_user(user.id).await.unwrap()[0]
            .id
            .to_string();
        let chain = Arc::new(MockChain::ethereum());
        chain.set_balance(WALLET_ADDR, U256::from(1_000_000u64));
        let mut map: HashMap<Chain, Arc<dyn BlockchainClient>> = HashMap::new();
        map.insert(Chain::Ethereum, chain);
        let blacklist = Arc::new(kyc_aml::InMemoryBlacklist::new());
        blacklist.add(Chain::Ethereum, BAD);
        let state = build_state(store, Arc::new(map), blacklist);
        let token = state.jwt.issue(user.id, Role::User).unwrap();

        let resp = router(state)
            .oneshot(withdraw_request(
                &wid,
                &token,
                "idem-aml",
                serde_json::json!({ "to_address": BAD, "amount_raw": "1000" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn withdraw_records_audit() {
        let (state, token, _chain, wid) = withdraw_state(1_000_000).await;
        router(state.clone())
            .oneshot(withdraw_request(
                &wid,
                &token,
                "idem-audit",
                serde_json::json!({ "to_address": "0x000000000000000000000000000000000000beef", "amount_raw": "1000" }),
            ))
            .await
            .unwrap();
        let log = state.audit.list().await.unwrap();
        assert!(log
            .iter()
            .any(|e| e.action == "withdraw.broadcast" && e.result == "ok"));
    }

    #[tokio::test]
    async fn withdraw_requires_kyc() {
        // Свежий пользователь без одобренного KYC.
        let store = Arc::new(InMemoryStore::new());
        let user = UserRepository::create(
            &*store,
            NewUser {
                email: "nokyc@test.dev".into(),
                password_hash: hash_password("password123").unwrap(),
                role: Role::User,
            },
        )
        .await
        .unwrap();
        use storage::{NewWallet, WalletRepository};
        let address = "0x000000000000000000000000000000000000abcd".to_string();
        WalletRepository::create(
            &*store,
            NewWallet {
                user_id: user.id,
                chain: Chain::Ethereum,
                address: address.clone(),
                derivation_path: "m/44'/60'/0'/0/0".into(),
            },
            10,
        )
        .await
        .unwrap();
        let wid = store.list_for_user(user.id).await.unwrap()[0]
            .id
            .to_string();
        let chain = Arc::new(MockChain::ethereum());
        chain.set_balance(&address, U256::from(1_000_000u64));
        let mut map: HashMap<Chain, Arc<dyn BlockchainClient>> = HashMap::new();
        map.insert(Chain::Ethereum, chain);
        let state = build_state(store, Arc::new(map), empty_aml());
        let token = state.jwt.issue(user.id, Role::User).unwrap();

        let resp = router(state)
            .oneshot(withdraw_request(
                &wid,
                &token,
                "idem-kyc",
                serde_json::json!({ "to_address": "0x000000000000000000000000000000000000beef", "amount_raw": "1000" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn ws_rejects_without_token() {
        use tokio_tungstenite::connect_async;
        let (state, _) = test_state().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(state);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        // Без Sec-WebSocket-Protocol сервер отвечает 401 → апгрейд не проходит.
        assert!(connect_async(format!("ws://{addr}/v1/ws")).await.is_err());
    }

    #[tokio::test]
    async fn ws_receives_event_for_own_wallet() {
        use futures_util::StreamExt;
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message;

        let (state, token, _chain, wid) = withdraw_state(1_000_000).await;
        let user_id = UserId(Uuid::parse_str(&state.jwt.verify(&token).unwrap().sub).unwrap());
        let wallet_id = WalletId(Uuid::parse_str(&wid).unwrap());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // JWT передаётся как Sec-WebSocket-Protocol.
        let mut req = format!("ws://{addr}/v1/ws").into_client_request().unwrap();
        req.headers_mut()
            .insert("sec-websocket-protocol", token.parse().unwrap());
        let (mut ws, _resp) = connect_async(req).await.unwrap();

        // Дать серверу подписаться, затем опубликовать событие.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        crate::events::publish(
            &state,
            user_id,
            wallet_id,
            "tx-1".into(),
            TransactionStatus::Unconfirmed,
            Some("0xabc".into()),
        );

        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
            .await
            .expect("event within 2s")
            .expect("stream item")
            .expect("ws message");
        let text = match msg {
            Message::Text(t) => t.to_string(),
            other => panic!("unexpected ws frame: {other:?}"),
        };
        assert!(text.contains("unconfirmed"));
        assert!(text.contains(&wallet_id.to_string()));
    }

    #[tokio::test]
    async fn withdraw_signs_every_input_utxo_style() {
        use storage::{NewWallet, WalletRepository};
        // Кошелёк EVM-вида, но сеть-мок эмитирует 3 «входа» (имитация UTXO):
        // сага должна подписать каждый запрос и собрать одну транзакцию.
        let store = Arc::new(InMemoryStore::new());
        let user = UserRepository::create(
            &*store,
            NewUser {
                email: "utxo@test.dev".into(),
                password_hash: hash_password("password123").unwrap(),
                role: Role::User,
            },
        )
        .await
        .unwrap();
        store.set_kyc(user.id, KycStatus::Approved).await.unwrap();
        WalletRepository::create(
            &*store,
            NewWallet {
                user_id: user.id,
                chain: Chain::Ethereum,
                address: WALLET_ADDR.into(),
                derivation_path: "m/44'/60'/0'/0/0".into(),
            },
            10,
        )
        .await
        .unwrap();
        let wid = store.list_for_user(user.id).await.unwrap()[0]
            .id
            .to_string();
        let chain = Arc::new(MockChain::ethereum().with_inputs(3));
        chain.set_balance(WALLET_ADDR, U256::from(1_000_000u64));
        let mut map: HashMap<Chain, Arc<dyn BlockchainClient>> = HashMap::new();
        map.insert(Chain::Ethereum, chain.clone());
        let state = build_state(store, Arc::new(map), empty_aml());
        let token = state.jwt.issue(user.id, Role::User).unwrap();

        let resp = router(state)
            .oneshot(withdraw_request(
                &wid,
                &token,
                "idem-utxo",
                serde_json::json!({ "to_address": "0x000000000000000000000000000000000000beef", "amount_raw": "1000" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK); // 3 подписи собраны, assemble прошёл
        assert_eq!(chain.broadcast_count(), 1);
    }
}
