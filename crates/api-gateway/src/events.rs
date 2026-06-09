//! Реал-тайм события: broadcast-канал + WebSocket `/v1/ws`.
//!
//! Аутентификация WS — JWT через `Sec-WebSocket-Protocol` (браузер не шлёт Authorization
//! при апгрейде). Сервер фильтрует поток по кошелькам аутентифицированного пользователя.
//! Источник истины — БД/REST; broadcast лишь доставляет live-уведомления.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use core_domain::{TransactionStatus, UserId, WalletId};
use serde::Serialize;
use tokio::sync::broadcast;

use crate::state::AppState;

/// Событие изменения состояния кошелька. `user_id` — для серверной фильтрации (наружу не идёт).
#[derive(Clone, Debug)]
pub struct WalletEvent {
    pub user_id: UserId,
    pub wallet_id: WalletId,
    pub tx_id: String,
    pub status: TransactionStatus,
    pub tx_hash: Option<String>,
}

/// Публичная проекция события (что уходит клиенту по WS).
#[derive(Serialize)]
struct WalletEventDto<'a> {
    wallet_id: String,
    tx_id: &'a str,
    status: TransactionStatus,
    tx_hash: Option<&'a str>,
}

pub fn channel() -> broadcast::Sender<WalletEvent> {
    broadcast::Sender::new(1024)
}

fn bearer_protocol(headers: &HeaderMap) -> Option<String> {
    headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or(s).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// `GET /v1/ws` — апгрейд с JWT в `Sec-WebSocket-Protocol`. Невалидный токен → 401.
pub async fn ws_handler(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
    headers: HeaderMap,
) -> Response {
    let token = match bearer_protocol(&headers) {
        Some(t) => t,
        None => return (StatusCode::UNAUTHORIZED, "missing token").into_response(),
    };
    let claims = match state.jwt.verify(&token) {
        Ok(c) => c,
        Err(_) => return (StatusCode::UNAUTHORIZED, "invalid token").into_response(),
    };
    let user_id = match uuid::Uuid::parse_str(&claims.sub) {
        Ok(id) => UserId(id),
        Err(_) => return (StatusCode::UNAUTHORIZED, "bad subject").into_response(),
    };
    // Эхо выбранного subprotocol (= токен), иначе строгие клиенты отклонят рукопожатие.
    let rx = state.events.subscribe();
    ws.protocols([token])
        .on_upgrade(move |socket| pump(socket, rx, user_id))
}

/// Пересылает события пользователя в сокет до отключения.
async fn pump(mut socket: WebSocket, mut rx: broadcast::Receiver<WalletEvent>, user: UserId) {
    loop {
        match rx.recv().await {
            Ok(ev) if ev.user_id == user => {
                let dto = WalletEventDto {
                    wallet_id: ev.wallet_id.to_string(),
                    tx_id: &ev.tx_id,
                    status: ev.status,
                    tx_hash: ev.tx_hash.as_deref(),
                };
                let json = serde_json::to_string(&dto).unwrap_or_default();
                if socket.send(Message::Text(json.into())).await.is_err() {
                    break; // клиент отключился
                }
            }
            Ok(_) => {} // чужое событие — пропускаем (приватность)
            Err(broadcast::error::RecvError::Lagged(_)) => {} // медленный клиент — продолжаем
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Опубликовать событие (best-effort: нет подписчиков — игнорируем).
pub fn publish(
    state: &AppState,
    user_id: UserId,
    wallet_id: WalletId,
    tx_id: String,
    status: TransactionStatus,
    tx_hash: Option<String>,
) {
    let _ = state.events.send(WalletEvent {
        user_id,
        wallet_id,
        tx_id,
        status,
        tx_hash,
    });
}
