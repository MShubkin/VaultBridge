import { describe, expect, it } from "vitest";
import { newIdempotencyKey, prepareWithdraw } from "./withdraw";
import { MoneyError } from "./money";

describe("newIdempotencyKey", () => {
  it("returns a uuid-shaped string", () => {
    const key = newIdempotencyKey();
    expect(key).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i,
    );
  });

  it("is unique per call", () => {
    expect(newIdempotencyKey()).not.toBe(newIdempotencyKey());
  });
});

describe("prepareWithdraw", () => {
  it("converts human amount to raw by decimals", () => {
    const r = prepareWithdraw("0.5", "1.5", 18);
    expect(r.amount_raw).toBe("1500000000000000000");
    expect(r.to_address).toBe("0.5"); // trimmed as-is (no validation here)
  });

  it("omits max_fee when not provided", () => {
    expect(prepareWithdraw("0xabc", "1", 8).max_fee_raw).toBeUndefined();
  });

  it("passes through max_fee when provided", () => {
    expect(prepareWithdraw("0xabc", "1", 8, "21000").max_fee_raw).toBe("21000");
  });

  it("rejects amount with too many fractional digits", () => {
    expect(() => prepareWithdraw("0xabc", "0.123456789", 8)).toThrow(MoneyError);
  });
});
