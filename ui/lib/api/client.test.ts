import { afterEach, describe, expect, it, vi } from "vitest";
import { apiFetch, ApiError } from "./client";

function mockFetch(status: number, body: unknown) {
  const fn = vi.fn((_input: string, _init?: RequestInit) =>
    Promise.resolve(
      new Response(JSON.stringify(body), {
        status,
        headers: { "content-type": "application/json" },
      }),
    ),
  );
  vi.stubGlobal("fetch", fn);
  return fn;
}

afterEach(() => vi.unstubAllGlobals());

describe("apiFetch", () => {
  it("returns parsed body on success", async () => {
    mockFetch(200, { ok: true });
    await expect(apiFetch<{ ok: boolean }>("/x")).resolves.toEqual({ ok: true });
  });

  it("attaches bearer token", async () => {
    const fn = mockFetch(200, {});
    await apiFetch("/x", { token: "tok123" });
    const init = fn.mock.calls[0][1];
    const headers = init?.headers as Record<string, string>;
    expect(headers.authorization).toBe("Bearer tok123");
  });

  it("maps error body to ApiError with code", async () => {
    mockFetch(401, { code: "unauthorized", message: "authentication required" });
    await expect(apiFetch("/x")).rejects.toMatchObject({
      status: 401,
      code: "unauthorized",
    });
  });

  it("throws ApiError even when body is not json", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("boom", { status: 500 })),
    );
    const err = await apiFetch("/x").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(500);
  });
});
