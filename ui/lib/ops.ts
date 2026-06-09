// Чистые помощники операторской консоли: агрегация выводов по статусам
// и цвет бейджа FSM. Без React/IO — юнит-тестируемо.

import type { OpsTx, TxStatus } from "@/lib/api/types";

/** Сводка числа выводов по статусам (для дашборда). */
export function countByStatus(txs: OpsTx[]): Record<string, number> {
  const out: Record<string, number> = {};
  for (const t of txs) {
    out[t.status] = (out[t.status] ?? 0) + 1;
  }
  return out;
}

/** Сколько выводов требуют внимания оператора (зависшие/протухшие/ошибки). */
export function needsAttention(txs: OpsTx[]): number {
  return txs.filter((t) =>
    (["failed", "expired", "replaced"] as TxStatus[]).includes(t.status),
  ).length;
}

/** Tailwind-классы бейджа по статусу (терминальные сбои — красный, успех — зелёный). */
export function statusBadgeClass(status: TxStatus): string {
  switch (status) {
    case "confirmed":
      return "bg-green-100 text-green-800";
    case "failed":
    case "expired":
      return "bg-red-100 text-red-800";
    case "replaced":
      return "bg-amber-100 text-amber-800";
    default:
      return "bg-neutral-100 text-neutral-700"; // created/signing/broadcast/unconfirmed
  }
}
