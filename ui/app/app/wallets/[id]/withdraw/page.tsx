"use client";

import { use, useState } from "react";
import { ApiError, apiFetch } from "@/lib/api/client";
import {
  CHAIN_DECIMALS,
  type QuoteResponse,
  type WithdrawResponse,
} from "@/lib/api/types";
import { useRequireAuth } from "@/lib/auth/guard";
import { useSession } from "@/lib/auth/session";
import { formatAmount, maxWithdrawable } from "@/lib/money";
import { newIdempotencyKey, prepareWithdraw } from "@/lib/withdraw";

// Этап U3: EVM-форма (decimals=18). Per-chain decimals подтянутся из данных кошелька позже.
const DECIMALS = CHAIN_DECIMALS.ethereum;

function errorMessage(err: unknown): string {
  if (err instanceof ApiError) {
    switch (err.status) {
      case 403:
        return "Вывод недоступен: требуется пройденный KYC.";
      case 404:
        return "Кошелёк не найден.";
      case 409:
        return "Операция уже выполняется. Повторите чуть позже.";
      case 422:
        return err.message || "Проверьте адрес и сумму.";
      case 429:
        return "Слишком много запросов. Повторите позже.";
      default:
        return "Не удалось выполнить операцию.";
    }
  }
  return err instanceof Error ? err.message : "Ошибка ввода.";
}

export default function WithdrawPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = use(params);
  const { ready, user } = useRequireAuth("user");
  const token = useSession((s) => s.token);

  const [toAddress, setToAddress] = useState("");
  const [amount, setAmount] = useState("");
  const [maxFee, setMaxFee] = useState("");
  const [quote, setQuote] = useState<QuoteResponse | null>(null);
  const [result, setResult] = useState<WithdrawResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  // Idempotency-Key фиксируется на время попытки и переживает ретраи.
  const [idemKey, setIdemKey] = useState<string>(() => newIdempotencyKey());

  if (!ready || !user) {
    return <main className="p-8 text-neutral-500">Загрузка…</main>;
  }

  async function getQuote() {
    setError(null);
    setResult(null);
    setPending(true);
    try {
      const body = prepareWithdraw(toAddress, amount, DECIMALS, maxFee || undefined);
      const q = await apiFetch<QuoteResponse>(
        `/v1/wallets/${id}/withdraw/quote`,
        { method: "POST", body, token },
      );
      setQuote(q);
    } catch (err) {
      setQuote(null);
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  }

  async function confirmWithdraw() {
    setError(null);
    setPending(true);
    try {
      const body = prepareWithdraw(toAddress, amount, DECIMALS, maxFee || undefined);
      const res = await apiFetch<WithdrawResponse>(`/v1/wallets/${id}/withdraw`, {
        method: "POST",
        body,
        token,
        headers: { "idempotency-key": idemKey },
      });
      setResult(res);
      setIdemKey(newIdempotencyKey()); // следующий вывод — новый ключ
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  }

  function setMax() {
    if (!quote) return;
    const max = maxWithdrawable(quote.spendable_raw, quote.estimated_fee_raw);
    setAmount(formatAmount(max, DECIMALS));
  }

  return (
    <main className="mx-auto max-w-lg p-8">
      <h1 className="text-2xl font-semibold">Вывод средств</h1>

      <div className="mt-4 flex flex-col gap-3">
        <input
          placeholder="Адрес назначения (0x…)"
          value={toAddress}
          onChange={(e) => setToAddress(e.target.value)}
          className="rounded border border-neutral-300 px-3 py-2 font-mono text-sm"
        />
        <div className="flex gap-2">
          <input
            placeholder="Сумма"
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
            className="flex-1 rounded border border-neutral-300 px-3 py-2"
          />
          <button
            onClick={setMax}
            disabled={!quote}
            className="rounded border border-neutral-300 px-3 text-sm disabled:opacity-50"
            title="Доступно за вычетом комиссии"
          >
            Max
          </button>
        </div>
        <input
          placeholder="Max fee (опционально, raw)"
          value={maxFee}
          onChange={(e) => setMaxFee(e.target.value)}
          className="rounded border border-neutral-300 px-3 py-2 text-sm"
        />

        <button
          onClick={getQuote}
          disabled={pending}
          className="rounded border border-blue-600 px-3 py-2 text-blue-600 disabled:opacity-50"
        >
          Получить оценку
        </button>
      </div>

      {quote && (
        <section className="mt-4 rounded bg-neutral-100 p-4 text-sm">
          <p>Комиссия: {formatAmount(quote.estimated_fee_raw, DECIMALS)}</p>
          <p>Итого к списанию: {formatAmount(quote.total_debit_raw, DECIMALS)}</p>
          <p>Доступно: {formatAmount(quote.spendable_raw, DECIMALS)}</p>
          <button
            onClick={confirmWithdraw}
            disabled={pending}
            className="mt-3 rounded bg-blue-600 px-3 py-2 font-medium text-white disabled:opacity-50"
          >
            {pending ? "Отправка…" : "Подтвердить вывод"}
          </button>
        </section>
      )}

      {error && <p className="mt-4 text-sm text-red-600">{error}</p>}
      {result && (
        <p className="mt-4 text-sm text-green-700">
          Отправлено. Статус: {result.status}
          {result.tx_hash ? `, tx: ${result.tx_hash}` : ""}
        </p>
      )}
    </main>
  );
}
