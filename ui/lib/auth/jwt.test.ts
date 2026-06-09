import { describe, expect, it } from "vitest";
import { decodeJwt, isExpired } from "./jwt";

// Хелпер: собрать неподписанный JWT с заданным payload (подпись не проверяется на клиенте).
function makeToken(payload: Record<string, unknown>): string {
  const b64 = (o: unknown) =>
    Buffer.from(JSON.stringify(o))
      .toString("base64")
      .replace(/\+/g, "-")
      .replace(/\//g, "_")
      .replace(/=+$/, "");
  return `${b64({ alg: "HS256", typ: "JWT" })}.${b64(payload)}.sig`;
}

describe("decodeJwt", () => {
  it("decodes sub/role/exp", () => {
    const token = makeToken({ sub: "u-1", role: "operator", exp: 9999999999 });
    const claims = decodeJwt(token);
    expect(claims).toEqual({ sub: "u-1", role: "operator", exp: 9999999999 });
  });

  it("defaults unknown role to user", () => {
    const token = makeToken({ sub: "u-1", role: "alien", exp: 1 });
    expect(decodeJwt(token)?.role).toBe("user");
  });

  it("returns null on malformed token", () => {
    expect(decodeJwt("garbage")).toBeNull();
    expect(decodeJwt("a.b")).toBeNull();
  });

  it("detects expiry", () => {
    const claims = { sub: "x", role: "user" as const, exp: 1000 };
    expect(isExpired(claims, 2000)).toBe(true);
    expect(isExpired(claims, 500)).toBe(false);
  });
});
