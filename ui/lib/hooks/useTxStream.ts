"use client";

// Подписка на реал-тайм события: на каждое событие инвалидируем
// кеш кошельков (истина — REST). Если WS недоступен — фолбэк на periodic polling.

import { useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";
import { useSession } from "@/lib/auth/session";
import { createEventStream } from "@/lib/ws";

const WS_URL = process.env.NEXT_PUBLIC_WS_URL ?? "ws://localhost:8080/v1/ws";
const POLL_INTERVAL_MS = 15_000;

export function useTxStream() {
  const token = useSession((s) => s.token);
  const qc = useQueryClient();

  useEffect(() => {
    if (!token) return;
    let pollId: ReturnType<typeof setInterval> | undefined;
    const refresh = () => {
      qc.invalidateQueries({ queryKey: ["wallets"] });
    };

    const stream = createEventStream({
      url: WS_URL,
      token,
      onEvent: refresh,
      // WS недоступен (прокси/файрвол) — деградируем до опроса.
      onUnavailable: () => {
        pollId = setInterval(refresh, POLL_INTERVAL_MS);
      },
    });

    return () => {
      stream.close();
      if (pollId) clearInterval(pollId);
    };
  }, [token, qc]);
}
