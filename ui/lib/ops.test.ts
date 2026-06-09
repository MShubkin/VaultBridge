import { describe, expect, it } from "vitest";
import type { OpsTx, TxStatus } from "@/lib/api/types";
import { countByStatus, needsAttention, statusBadgeClass } from "./ops";

function tx(status: TxStatus): OpsTx {
  return {
    id: "t",
    wallet_id: "w",
    chain: "ethereum",
    to_address: "0x",
    amount_raw: "1",
    status,
    tx_hash: null,
    created_at_unix: 0,
  };
}

describe("countByStatus", () => {
  it("counts per status", () => {
    const c = countByStatus([tx("unconfirmed"), tx("unconfirmed"), tx("confirmed")]);
    expect(c.unconfirmed).toBe(2);
    expect(c.confirmed).toBe(1);
  });

  it("empty for no txs", () => {
    expect(countByStatus([])).toEqual({});
  });
});

describe("needsAttention", () => {
  it("counts failed/expired/replaced only", () => {
    const n = needsAttention([
      tx("failed"),
      tx("expired"),
      tx("replaced"),
      tx("confirmed"),
      tx("unconfirmed"),
    ]);
    expect(n).toBe(3);
  });
});

describe("statusBadgeClass", () => {
  it("maps terminal failure to red, success to green, replaced to amber", () => {
    expect(statusBadgeClass("confirmed")).toContain("green");
    expect(statusBadgeClass("failed")).toContain("red");
    expect(statusBadgeClass("expired")).toContain("red");
    expect(statusBadgeClass("replaced")).toContain("amber");
    expect(statusBadgeClass("unconfirmed")).toContain("neutral");
  });
});
