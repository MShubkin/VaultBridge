"use client";

// Клиентский маршрут-гард: после регидратации редиректит
// неаутентифицированных на /login и проверяет роль. Истинная авторизация — на бэке.

import { useRouter } from "next/navigation";
import { useEffect } from "react";
import type { Role } from "@/lib/api/types";
import { useSession } from "@/lib/auth/session";

export function useRequireAuth(role?: Role) {
  const router = useRouter();
  const ready = useSession((s) => s.ready);
  const user = useSession((s) => s.user);

  useEffect(() => {
    if (!ready) return;
    if (!user) {
      router.replace("/login");
      return;
    }
    if (role && user.role !== role) {
      router.replace(user.role === "operator" ? "/console" : "/app");
    }
  }, [ready, user, role, router]);

  return { ready, user };
}
