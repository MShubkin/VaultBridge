// WebSocket-клиент реал-тайм событий: JWT через subprotocol,
// реконнект с backoff, фолбэк на polling если WS недоступен. Истина — REST/БД;
// WS лишь триггерит инвалидацию кеша.

import type { TxStatus } from "@/lib/api/types";

export interface WalletEvent {
  wallet_id: string;
  tx_id: string;
  status: TxStatus;
  tx_hash: string | null;
}

const TX_STATUSES: readonly string[] = [
  "created",
  "signing",
  "broadcast",
  "unconfirmed",
  "confirmed",
  "failed",
  "expired",
  "replaced",
];

/** Распарсить кадр WS в WalletEvent; null на мусоре/неизвестном статусе. */
export function parseWalletEvent(raw: string): WalletEvent | null {
  try {
    const o = JSON.parse(raw) as Record<string, unknown>;
    if (
      typeof o.wallet_id !== "string" ||
      typeof o.tx_id !== "string" ||
      typeof o.status !== "string" ||
      !TX_STATUSES.includes(o.status)
    ) {
      return null;
    }
    return {
      wallet_id: o.wallet_id,
      tx_id: o.tx_id,
      status: o.status as TxStatus,
      tx_hash: typeof o.tx_hash === "string" ? o.tx_hash : null,
    };
  } catch {
    return null;
  }
}

/** Экспоненциальный backoff с потолком (мс). attempt 0,1,2… → 0.5s,1s,2s,… ≤ max. */
export function backoffMs(attempt: number, base = 500, max = 15_000): number {
  return Math.min(base * 2 ** Math.max(0, attempt), max);
}

export interface EventStreamOptions {
  url: string;
  token: string;
  onEvent: (ev: WalletEvent) => void;
  /** Вызывается, если WS недоступен совсем — потребитель включает polling. */
  onUnavailable?: () => void;
}

export interface EventStream {
  close(): void;
}

/**
 * Открыть поток событий с реконнектом. JWT передаётся как subprotocol.
 * Если `WebSocket` отсутствует (или окружение без него) — зовётся `onUnavailable`.
 */
export function createEventStream(opts: EventStreamOptions): EventStream {
  if (typeof WebSocket === "undefined") {
    opts.onUnavailable?.();
    return { close() {} };
  }

  let closed = false;
  let attempt = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let socket: WebSocket | undefined;

  const connect = () => {
    if (closed) return;
    // Токен — subprotocol (браузер не шлёт Authorization при апгрейде).
    socket = new WebSocket(opts.url, opts.token);
    socket.onopen = () => {
      attempt = 0;
    };
    socket.onmessage = (e) => {
      const ev = parseWalletEvent(String(e.data));
      if (ev) opts.onEvent(ev);
    };
    socket.onerror = () => socket?.close();
    socket.onclose = () => {
      if (closed) return;
      timer = setTimeout(connect, backoffMs(attempt++));
    };
  };

  connect();
  return {
    close() {
      closed = true;
      if (timer) clearTimeout(timer);
      socket?.close();
    },
  };
}
