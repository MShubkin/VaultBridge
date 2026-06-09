// Декодирование payload JWT БЕЗ проверки подписи — только чтение claims на клиенте
// (роль для редиректа/гарда, exp для регидратации). Истинная проверка — на бэке.

import type { Role } from "@/lib/api/types";

export interface JwtClaims {
  sub: string;
  role: Role;
  exp: number; // unix seconds
}

function base64UrlDecode(input: string): string {
  const pad = input.length % 4 === 0 ? "" : "=".repeat(4 - (input.length % 4));
  const b64 = input.replace(/-/g, "+").replace(/_/g, "/") + pad;
  if (typeof atob === "function") return atob(b64);
  // Node-окружение (тесты)
  return Buffer.from(b64, "base64").toString("binary");
}

export function decodeJwt(token: string): JwtClaims | null {
  const parts = token.split(".");
  if (parts.length !== 3) return null;
  try {
    const payload = JSON.parse(base64UrlDecode(parts[1])) as Partial<JwtClaims>;
    if (typeof payload.sub !== "string" || typeof payload.exp !== "number") {
      return null;
    }
    const role: Role = payload.role === "operator" ? "operator" : "user";
    return { sub: payload.sub, role, exp: payload.exp };
  } catch {
    return null;
  }
}

export function isExpired(claims: JwtClaims, nowSeconds = Date.now() / 1000): boolean {
  return claims.exp <= nowSeconds;
}
