import { describe, expect, it } from "vitest";
import { formatAmount, maxWithdrawable, MoneyError, parseAmount } from "./index";

describe("parseAmount", () => {
  it("parses whole and fractional human input to raw bigint", () => {
    expect(parseAmount("1", 18)).toBe(1_000000000000000000n);
    expect(parseAmount("0.5", 18)).toBe(500000000000000000n);
    expect(parseAmount("1.25", 8)).toBe(125000000n);
  });

  it("rejects more fractional digits than decimals (no silent truncation)", () => {
    expect(() => parseAmount("0.123456789", 8)).toThrow(MoneyError);
  });

  it("rejects non-numeric input", () => {
    expect(() => parseAmount("abc", 18)).toThrow(MoneyError);
    expect(() => parseAmount("1,5", 18)).toThrow(MoneyError);
  });
});

describe("formatAmount", () => {
  it("scales by decimals and trims trailing zeros", () => {
    expect(formatAmount("1000000000000000000", 18)).toBe("1");
    expect(formatAmount("1500000000000000000", 18)).toBe("1.5");
    expect(formatAmount(125000000n, 8)).toBe("1.25");
  });

  it("groups large whole parts", () => {
    expect(formatAmount("1234000000000000000000", 18)).toBe("1,234");
  });

  it("round-trips with parseAmount", () => {
    const raw = parseAmount("12.34", 9);
    expect(formatAmount(raw, 9)).toBe("12.34");
  });
});

describe("maxWithdrawable", () => {
  it("subtracts fee from spendable", () => {
    expect(maxWithdrawable("1000", "100")).toBe(900n);
  });

  it("never goes negative", () => {
    expect(maxWithdrawable("50", "100")).toBe(0n);
  });
});
