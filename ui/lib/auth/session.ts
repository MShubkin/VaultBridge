// Сессия: access-token ТОЛЬКО в памяти (не localStorage — защита от
// XSS-кражи). refresh-token живёт в httpOnly-cookie и здесь недоступен.

import { create } from "zustand";
import { apiFetch } from "@/lib/api/client";
import type { LoginResponse, Role } from "@/lib/api/types";
import { decodeJwt } from "@/lib/auth/jwt";

export interface SessionUser {
  id: string;
  role: Role;
  exp: number;
}

interface SessionState {
  token: string | null;
  user: SessionUser | null;
  /** false до завершения регидратации на старте. */
  ready: boolean;
  setToken: (token: string) => void;
  clear: () => void;
  setReady: (ready: boolean) => void;
  login: (email: string, password: string) => Promise<SessionUser>;
}

export const useSession = create<SessionState>((set) => ({
  token: null,
  user: null,
  ready: false,
  setToken: (token) => {
    const claims = decodeJwt(token);
    set({
      token,
      user: claims ? { id: claims.sub, role: claims.role, exp: claims.exp } : null,
    });
  },
  clear: () => set({ token: null, user: null }),
  setReady: (ready) => set({ ready }),
  login: async (email, password) => {
    const res = await apiFetch<LoginResponse>("/v1/auth/login", {
      method: "POST",
      body: { email, password },
    });
    const claims = decodeJwt(res.access_token);
    if (!claims) throw new Error("invalid token");
    const user: SessionUser = { id: claims.sub, role: claims.role, exp: claims.exp };
    set({ token: res.access_token, user });
    return user;
  },
}));
