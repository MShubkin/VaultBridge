"use client";

import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "@/lib/api/client";
import type { OpsAuditEntry } from "@/lib/api/types";
import { useRequireAuth } from "@/lib/auth/guard";
import { useSession } from "@/lib/auth/session";

// Аудит-лог, read-only (роль operator).
export default function OpsAuditPage() {
  const { ready, user } = useRequireAuth("operator");
  const token = useSession((s) => s.token);

  const log = useQuery({
    queryKey: ["ops", "audit"],
    enabled: ready && user?.role === "operator" && !!token,
    refetchInterval: 15_000,
    queryFn: () => apiFetch<OpsAuditEntry[]>("/v1/ops/audit", { token }),
  });

  if (!ready || user?.role !== "operator") {
    return <main className="p-8 text-neutral-500">Загрузка…</main>;
  }

  return (
    <main className="mx-auto max-w-4xl p-8">
      <a href="/console" className="text-sm text-blue-600 underline">
        ← Консоль
      </a>
      <h1 className="mt-2 text-2xl font-semibold">Аудит-лог</h1>
      <p className="text-xs text-neutral-500">Append-only; запись неизменяема.</p>

      {log.isError && <p className="mt-4 text-red-600">Ошибка загрузки.</p>}

      <ul className="mt-4 divide-y divide-neutral-200 text-sm">
        {log.data?.map((e) => (
          <li key={e.id} className="flex items-center justify-between py-2">
            <span className="font-mono">{e.action}</span>
            <span
              className={
                e.result === "ok"
                  ? "text-green-700"
                  : e.result === "denied"
                    ? "text-amber-700"
                    : "text-red-700"
              }
            >
              {e.result}
            </span>
            <span className="text-neutral-500">
              {new Date(e.created_at_unix * 1000).toLocaleString()}
            </span>
          </li>
        ))}
      </ul>
      {log.data?.length === 0 && <p className="mt-4 text-neutral-500">Записей нет.</p>}
    </main>
  );
}
