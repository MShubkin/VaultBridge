//! Метрики: лёгкие счётчики в Prometheus-формате, без глобального
//! рекордера (чтобы не конфликтовать в тестах). В проде заменяется на metrics-exporter.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct Metrics {
    withdraw_ok: AtomicU64,
    withdraw_denied: AtomicU64,
    withdraw_error: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Учесть исход вывода по результату аудита (ok|denied|error).
    pub fn record_withdraw(&self, result: &str) {
        let counter = match result {
            "ok" => &self.withdraw_ok,
            "denied" => &self.withdraw_denied,
            _ => &self.withdraw_error,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Экспозиция в Prometheus text format.
    pub fn render(&self) -> String {
        let ok = self.withdraw_ok.load(Ordering::Relaxed);
        let denied = self.withdraw_denied.load(Ordering::Relaxed);
        let error = self.withdraw_error.load(Ordering::Relaxed);
        format!(
            "# HELP vaultbridge_withdrawals_total Withdrawal outcomes.\n\
             # TYPE vaultbridge_withdrawals_total counter\n\
             vaultbridge_withdrawals_total{{result=\"ok\"}} {ok}\n\
             vaultbridge_withdrawals_total{{result=\"denied\"}} {denied}\n\
             vaultbridge_withdrawals_total{{result=\"error\"}} {error}\n"
        )
    }
}
