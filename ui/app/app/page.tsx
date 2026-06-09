"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "@/lib/api/client";
import type { Chain, WalletDto } from "@/lib/api/types";
import { useRequireAuth } from "@/lib/auth/guard";
import { useSession } from "@/lib/auth/session";
import { useTxStream } from "@/lib/hooks/useTxStream";

const CHAINS: Chain[] = ["ethereum", "bitcoin", "solana"];

export default function WalletOverviewPage() {
  const { ready, user } = useRequireAuth("user");
  const token = useSession((s) => s.token);
  const qc = useQueryClient();
  // Live-обновления: WS-события инвалидируют список кошельков.
  useTxStream();

  const wallets = useQuery({
    queryKey: ["wallets"],
    enabled: ready && !!user && !!token,
    queryFn: () => apiFetch<WalletDto[]>("/v1/wallets", { token }),
  });

  const createWallet = useMutation({
    mutationFn: (chain: Chain) =>
      apiFetch<WalletDto>("/v1/wallets", { method: "POST", body: { chain }, token }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["wallets"] }),
  });

  if (!ready || !user) {
    return <main className="p-8 text-neutral-500">Загрузка…</main>;
  }

  return (
    <main className="mx-auto max-w-3xl p-8">
      <h1 className="text-2xl font-semibold">Мои кошельки</h1>

      <div className="mt-4 flex gap-2">
        {CHAINS.map((c) => (
          <button
            key={c}
            onClick={() => createWallet.mutate(c)}
            disabled={createWallet.isPending}
            className="rounded border border-neutral-300 px-3 py-1.5 text-sm capitalize hover:bg-neutral-100 disabled:opacity-50"
          >
            + {c}
          </button>
        ))}
      </div>

      <section className="mt-6">
        {wallets.isLoading && <p className="text-neutral-500">Загрузка кошельков…</p>}
        {wallets.isError && (
          <p className="text-red-600">Не удалось загрузить кошельки.</p>
        )}
        {wallets.data && wallets.data.length === 0 && (
          <p className="text-neutral-500">Кошельков пока нет — создайте первый.</p>
        )}
        <ul className="divide-y divide-neutral-200">
          {wallets.data?.map((w) => (
            <li key={w.id} className="flex items-center justify-between gap-4 py-3">
              <span className="font-medium capitalize">{w.chain}</span>
              <code className="flex-1 truncate text-sm text-neutral-600">{w.address}</code>
              <a
                href={`/app/wallets/${w.id}/withdraw`}
                className="text-sm text-blue-600 underline"
              >
                Вывести
              </a>
            </li>
          ))}
        </ul>
      </section>
    </main>
  );
}
