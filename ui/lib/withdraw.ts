// Логика формы вывода: генерация и персист Idempotency-Key,
// перевод суммы human → raw. Idempotency-Key переживает ретраи до терминального ответа.

import { parseAmount } from "@/lib/money";

/** UUID v4 как Idempotency-Key. */
export function newIdempotencyKey(): string {
  return crypto.randomUUID();
}

export interface PreparedWithdraw {
  to_address: string;
  amount_raw: string;
  max_fee_raw?: string;
}

/**
 * Подготовить тело запроса вывода: human-сумма → raw по decimals (бросает исключение при
 * лишних знаках или мусоре). max_fee опционален — это потолок против проскальзывания комиссии.
 */
export function prepareWithdraw(
  toAddress: string,
  amountHuman: string,
  decimals: number,
  maxFeeRaw?: string,
): PreparedWithdraw {
  const amount = parseAmount(amountHuman, decimals);
  return {
    to_address: toAddress.trim(),
    amount_raw: amount.toString(),
    max_fee_raw: maxFeeRaw && maxFeeRaw.length > 0 ? maxFeeRaw : undefined,
  };
}
