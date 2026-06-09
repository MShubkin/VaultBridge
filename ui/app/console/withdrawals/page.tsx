"use client";

import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "@/lib/api/client";
import { CHAIN_DECIMALS, type OpsTx } from "@/lib/api/types";
import { useRequireAuth } from "@/lib/auth/guard";
import { useSession } from "@/lib/auth/session";
import { formatAmount } from "@/lib/money";
import { statusBadgeClass } from "@/lib/ops";

// Все выводы по всем пользователям (роль operator).
export default function OpsWithdrawalsPage() {
  const { ready, user } = useRequireAuth("operator");
  const token = useSession((s) => s.token);

  const txs = useQuery({
    queryKey: ["ops", "withdrawals"],
    enabled: ready && user?.role === "operator" && !!token,
    refetchInterval: 15_000,
    queryFn: () => apiFetch<OpsTx[]>("/v1/ops/withdrawals", { token }),
  });

  if (!ready || user?.role !== "operator") {
    return <main className="p-8 text-neutral-500">Загрузка…</main>;
  }

  return (
    <main className="mx-auto max-w-5xl p-8">
      <a href="/console" className="text-sm text-blue-600 underline">
        ← Консоль
      </a>
      <h1 className="mt-2 text-2xl font-semibold">Выводы</h1>

      {txs.isLoading && <p className="mt-4 text-neutral-500">Загрузка…</p>}
      {txs.isError && <p className="mt-4 text-red-600">Ошибка загрузки.</p>}

      <table className="mt-4 w-full text-sm">
        <thead className="text-left text-neutral-500">
          <tr>
            <th className="py-2">Сеть</th>
            <th>Сумма</th>
            <th>Адрес</th>
            <th>Статус</th>
            <th>Tx</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-neutral-200">
          {txs.data?.map((t) => (
            <tr key={t.id}>
              <td className="py-2 capitalize">{t.chain}</td>
              <td>{formatAmount(t.amount_raw, CHAIN_DECIMALS[t.chain])}</td>
              <td className="max-w-[12rem] truncate font-mono text-neutral-600">
                {t.to_address ?? "—"}
              </td>
              <td>
                <span className={`rounded px-2 py-0.5 text-xs ${statusBadgeClass(t.status)}`}>
                  {t.status}
                </span>
              </td>
              <td className="max-w-[10rem] truncate font-mono text-neutral-500">
                {t.tx_hash ?? "—"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {txs.data?.length === 0 && <p className="mt-4 text-neutral-500">Выводов пока нет.</p>}
    </main>
  );
}
