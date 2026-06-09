"use client";

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { useSession } from "@/lib/auth/session";

// Регидратация сессии на старте: здесь будет silent-refresh по
// httpOnly refresh-cookie. Пока бэкенд не выдаёт refresh-cookie — помечаем готовность
// сразу (сессии нет до явного логина). Реальный refresh подключится с эндпоинтом /auth/refresh.
function useRehydrate() {
  const setReady = useSession((s) => s.setReady);
  useEffect(() => {
    // TODO(stage 1): POST /v1/auth/refresh по cookie → setToken; при ошибке — без сессии.
    setReady(true);
  }, [setReady]);
}

export function Providers({ children }: { children: React.ReactNode }) {
  const [queryClient] = useState(() => new QueryClient());
  useRehydrate();
  return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
}
