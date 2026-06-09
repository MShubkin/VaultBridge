"use client";

import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "@/lib/api/client";
import type { OpsTx } from "@/lib/api/types";
import { useRequireAuth } from "@/lib/auth/guard";
import { useSession } from "@/lib/auth/session";
import { countByStatus, needsAttention } from "@/lib/ops";

// Операторская консоль: данные из /v1/ops/* (роль operator). Обновление — опросом.
export default function ConsolePage() {
  const { ready, user } = useRequireAuth("operator");
  const token = useSession((s) => s.token);

  const txs = useQuery({
    queryKey: ["ops", "withdrawals"],
    enabled: ready && user?.role === "operator" && !!token,
    refetchInterval: 15_000, // операторский вид обновляется опросом
    queryFn: () => apiFetch<OpsTx[]>("/v1/ops/withdrawals", { token }),
  });

  if (!ready || user?.role !== "operator") {
    return <main className="p-8 text-neutral-500">Загрузка…</main>;
  }

  const counts = countByStatus(txs.data ?? []);
  const attention = needsAttention(txs.data ?? []);

  return (
    <main className="mx-auto max-w-4xl p-8">
      <h1 className="text-2xl font-semibold">Операторская консоль</h1>
      <nav className="mt-2 flex gap-4 text-sm text-blue-600 underline">
        <a href="/console/withdrawals">Выводы</a>
        <a href="/console/audit">Аудит</a>
      </nav>

      <section className="mt-6 grid grid-cols-2 gap-4 sm:grid-cols-4">
        <Card label="Всего выводов" value={(txs.data ?? []).length} />
        <Card label="Требуют внимания" value={attention} highlight={attention > 0} />
        <Card label="Unconfirmed" value={counts.unconfirmed ?? 0} />
        <Card label="Confirmed" value={counts.confirmed ?? 0} />
      </section>

      {txs.isError && (
        <p className="mt-4 text-sm text-red-600">Не удалось загрузить данные операций.</p>
      )}
    </main>
  );
}

function Card({
  label,
  value,
  highlight,
}: {
  label: string;
  value: number;
  highlight?: boolean;
}) {
  return (
    <div
      className={`rounded border p-4 ${highlight ? "border-red-300 bg-red-50" : "border-neutral-200"}`}
    >
      <div className="text-2xl font-semibold">{value}</div>
      <div className="text-xs text-neutral-500">{label}</div>
    </div>
  );
}
