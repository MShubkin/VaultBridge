// Корень: на этапе 0 — статичная посадочная заглушка.
// Редирект по роли (user → /app, operator → /console) добавляется на этапе U1.
export default function Home() {
  return (
    <main className="mx-auto flex min-h-screen max-w-2xl flex-col justify-center gap-4 p-8">
      <h1 className="text-3xl font-semibold">VaultBridge</h1>
      <p className="text-neutral-600">
        Кастодиальный мультиблокчейн-кошелёк. Каркас фронтенда (этап 0).
      </p>
      <nav className="flex gap-4 text-blue-600 underline">
        <a href="/login">Login</a>
        <a href="/app">Wallet</a>
        <a href="/console">Console</a>
      </nav>
    </main>
  );
}
