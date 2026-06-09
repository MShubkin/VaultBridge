import { describe, expect, it } from "vitest";
import { backoffMs, parseWalletEvent } from "./ws";

describe("parseWalletEvent", () => {
  it("parses a valid event", () => {
    const ev = parseWalletEvent(
      JSON.stringify({
        wallet_id: "w1",
        tx_id: "t1",
        status: "unconfirmed",
        tx_hash: "0xabc",
      }),
    );
    expect(ev).toEqual({
      wallet_id: "w1",
      tx_id: "t1",
      status: "unconfirmed",
      tx_hash: "0xabc",
    });
  });

  it("allows null tx_hash", () => {
    const ev = parseWalletEvent(
      JSON.stringify({ wallet_id: "w", tx_id: "t", status: "created", tx_hash: null }),
    );
    expect(ev?.tx_hash).toBeNull();
  });

  it("rejects unknown status", () => {
    expect(
      parseWalletEvent(JSON.stringify({ wallet_id: "w", tx_id: "t", status: "weird" })),
    ).toBeNull();
  });

  it("rejects malformed json / missing fields", () => {
    expect(parseWalletEvent("not json")).toBeNull();
    expect(parseWalletEvent(JSON.stringify({ wallet_id: "w" }))).toBeNull();
  });
});

describe("backoffMs", () => {
  it("grows exponentially from base", () => {
    expect(backoffMs(0)).toBe(500);
    expect(backoffMs(1)).toBe(1000);
    expect(backoffMs(2)).toBe(2000);
  });

  it("caps at max", () => {
    expect(backoffMs(20)).toBe(15_000);
  });

  it("treats negative attempt as 0", () => {
    expect(backoffMs(-5)).toBe(500);
  });
});
