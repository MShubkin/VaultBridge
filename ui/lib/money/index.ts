// Деньги на клиенте — только bigint. Number/float запрещён.
// U256 пересекает JSON строкой; на клиенте парсится в bigint на краю API-слоя.

export type RawAmount = string; // минимальные единицы, напр. "1000000000000000000"

export class MoneyError extends Error {}

/**
 * Человекочитаемый ввод ("0.5") → raw bigint в минимальных единицах.
 * Отклоняет больше знаков после точки, чем decimals (иначе тихая потеря точности).
 */
export function parseAmount(input: string, decimals: number): bigint {
  const trimmed = input.trim();
  if (!/^\d+(\.\d+)?$/.test(trimmed)) {
    throw new MoneyError(`invalid amount: ${input}`);
  }
  const [whole, frac = ""] = trimmed.split(".");
  if (frac.length > decimals) {
    throw new MoneyError(
      `too many fractional digits: ${frac.length} > ${decimals}`,
    );
  }
  const padded = frac.padEnd(decimals, "0");
  return BigInt(whole) * 10n ** BigInt(decimals) + BigInt(padded || "0");
}

/**
 * raw bigint/строка → человекочитаемая строка с десятичной точкой.
 * Сам делит на 10^decimals (Intl не масштабирует) и группирует разряды целой части.
 */
export function formatAmount(raw: RawAmount | bigint, decimals: number): string {
  const value = typeof raw === "bigint" ? raw : BigInt(raw);
  const base = 10n ** BigInt(decimals);
  const whole = value / base;
  const frac = value % base;
  const groupedWhole = new Intl.NumberFormat("en-US").format(whole);
  if (decimals === 0 || frac === 0n) return groupedWhole;
  const fracStr = frac.toString().padStart(decimals, "0").replace(/0+$/, "");
  return `${groupedWhole}.${fracStr}`;
}

/** Доступно к выводу с учётом комиссии: spendable − fee (за этим стоит кнопка «Max»). */
export function maxWithdrawable(spendableRaw: RawAmount, feeRaw: RawAmount): bigint {
  const spendable = BigInt(spendableRaw);
  const fee = BigInt(feeRaw);
  return spendable > fee ? spendable - fee : 0n;
}
