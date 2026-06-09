//! Доменное ядро VaultBridge — типы, которые знают все остальные крейты.
//!
//! Здесь нет ни сети, ни базы, ни HTTP: только идентификаторы, перечисления состояний
//! и денежная величина `Amount`. Всё остальное (`storage`, `blockchain`, `api-gateway`)
//! строится поверх этих типов, поэтому держим слой максимально чистым и без зависимостей
//! от инфраструктуры.

/// Денежный примитив `U256` нужен почти всем downstream-крейтам, поэтому ре-экспортируем
/// его отсюда — чтобы версия `ruint` была единой на весь воркспейс.
pub use ruint::aliases::U256;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use ids::{TransactionId, UserId, WalletId};

/// Сеть, которую мы поддерживаем. Это дискриминатор: по нему выбирается нужный
/// адаптер блокчейна, путь деривации ключа и формат адреса.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Chain {
    /// EVM-совместимая сеть (account-модель, durable nonce).
    Ethereum,
    /// Bitcoin (UTXO-модель).
    Bitcoin,
    /// Solana (account-модель с эфемерным recent blockhash).
    Solana,
}

impl Chain {
    /// Строковый код сети. Он же лежит в БД (CHECK-констрейнты) и ходит по gRPC —
    /// поэтому значения зафиксированы и менять их нельзя без миграции данных.
    pub fn as_str(self) -> &'static str {
        match self {
            Chain::Ethereum => "ethereum",
            Chain::Bitcoin => "bitcoin",
            Chain::Solana => "solana",
        }
    }

    /// Разобрать сеть из строкового кода; `None` на неизвестном значении.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ethereum" => Some(Chain::Ethereum),
            "bitcoin" => Some(Chain::Bitcoin),
            "solana" => Some(Chain::Solana),
            _ => None,
        }
    }
}

impl std::fmt::Display for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Статус прохождения KYC. Это именно перечисление, а не булев флаг: «проверку не
/// проходил» и «проверку завалил» — разные состояния, и путать их нельзя.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KycStatus {
    /// Заявка подана, решения ещё нет.
    Pending,
    /// Проверка пройдена — операции разрешены.
    Approved,
    /// Проверка отклонена.
    Rejected,
}

impl KycStatus {
    /// Можно ли выводить средства при таком статусе. Единственная точка правды для
    /// KYC-гейта, чтобы условие не расползалось по хендлерам.
    pub fn can_withdraw(self) -> bool {
        matches!(self, KycStatus::Approved)
    }

    /// Строковый код для хранения в БД и выдачи наружу.
    pub fn as_str(self) -> &'static str {
        match self {
            KycStatus::Pending => "pending",
            KycStatus::Approved => "approved",
            KycStatus::Rejected => "rejected",
        }
    }

    /// Разобрать статус из строки; `None` на неизвестном значении.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(KycStatus::Pending),
            "approved" => Some(KycStatus::Approved),
            "rejected" => Some(KycStatus::Rejected),
            _ => None,
        }
    }
}

/// Роль доступа для RBAC.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Обычный клиент: видит и распоряжается только своими кошельками.
    User,
    /// Оператор: межпользовательское чтение и разбор, без доступа к ключам.
    Operator,
}

/// Направление движения средств по кошельку.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Поступление на кошелёк.
    Incoming,
    /// Списание с кошелька (вывод).
    Outgoing,
}

impl Direction {
    /// Строковый код для хранения в БД.
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Incoming => "incoming",
            Direction::Outgoing => "outgoing",
        }
    }
    /// Разобрать направление из строки; `None` на неизвестном значении.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "incoming" => Some(Direction::Incoming),
            "outgoing" => Some(Direction::Outgoing),
            _ => None,
        }
    }
}

/// Жизненный цикл транзакции как конечный автомат. Терминальные состояния, из которых
/// переходов уже нет: `confirmed`, `failed`, `expired`, `replaced`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransactionStatus {
    /// Запись создана, операция ещё ничего не сделала в сети.
    Created,
    /// Идёт подпись (запрос ушёл в signing-service).
    Signing,
    /// Подписанная транзакция отправлена в сеть.
    Broadcast,
    /// Сеть приняла транзакцию, но подтверждений ещё недостаточно.
    Unconfirmed,
    /// Достигнут порог подтверждений — операция финализирована.
    Confirmed,
    /// Транзакция отклонена сетью или сорвалась до отправки.
    Failed,
    /// Истёк срок жизни (например, протух recent blockhash у Solana).
    Expired,
    /// Заменена другой транзакцией (bump gas / RBF).
    Replaced,
}

impl TransactionStatus {
    /// Строковый код для хранения в БД.
    pub fn as_str(self) -> &'static str {
        match self {
            TransactionStatus::Created => "created",
            TransactionStatus::Signing => "signing",
            TransactionStatus::Broadcast => "broadcast",
            TransactionStatus::Unconfirmed => "unconfirmed",
            TransactionStatus::Confirmed => "confirmed",
            TransactionStatus::Failed => "failed",
            TransactionStatus::Expired => "expired",
            TransactionStatus::Replaced => "replaced",
        }
    }
    /// Разобрать статус из строки; `None` на неизвестном значении.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "created" => TransactionStatus::Created,
            "signing" => TransactionStatus::Signing,
            "broadcast" => TransactionStatus::Broadcast,
            "unconfirmed" => TransactionStatus::Unconfirmed,
            "confirmed" => TransactionStatus::Confirmed,
            "failed" => TransactionStatus::Failed,
            "expired" => TransactionStatus::Expired,
            "replaced" => TransactionStatus::Replaced,
            _ => return None,
        })
    }
}

impl Role {
    /// Строковый код роли — кладётся в JWT-claim и в БД.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Operator => "operator",
        }
    }
    /// Разобрать роль из строки; `None` на неизвестном значении.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Role::User),
            "operator" => Some(Role::Operator),
            _ => None,
        }
    }
}

/// Что может пойти не так в арифметике денег.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum AmountError {
    /// Попытка сложить/вычесть суммы из разных сетей — это бессмыслица.
    #[error("amount chain mismatch: {lhs} vs {rhs}")]
    ChainMismatch { lhs: Chain, rhs: Chain },
    /// Переполнение или уход в минус (вычли больше, чем было).
    #[error("amount arithmetic overflow")]
    Overflow,
}

/// Деньги в минимальных единицах сети: wei, satoshi, lamports.
///
/// Принципиально не `f64` — у денег не бывает «почти». Внутри `U256`, которого хватает
/// и на wei (18 знаков), и на satoshi, и на lamports. Вся арифметика checked, а операции
/// разрешены только между суммами одной сети — иначе легко случайно сложить wei с satoshi.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Amount {
    /// Сеть, к которой относится сумма (она же задаёт смысл минимальной единицы).
    pub chain: Chain,
    /// Величина в минимальных единицах сети.
    pub raw: U256,
}

impl Amount {
    /// Собрать сумму из сети и сырого значения.
    pub fn new(chain: Chain, raw: U256) -> Self {
        Self { chain, raw }
    }

    /// Нулевая сумма для заданной сети.
    pub fn zero(chain: Chain) -> Self {
        Self {
            chain,
            raw: U256::ZERO,
        }
    }

    /// Равна ли сумма нулю.
    pub fn is_zero(&self) -> bool {
        self.raw == U256::ZERO
    }

    /// Сложение: сначала сверяем сеть, затем складываем с проверкой переполнения.
    pub fn checked_add(&self, other: &Amount) -> Result<Amount, AmountError> {
        self.ensure_same_chain(other)?;
        let raw = self
            .raw
            .checked_add(other.raw)
            .ok_or(AmountError::Overflow)?;
        Ok(Amount {
            chain: self.chain,
            raw,
        })
    }

    /// Вычитание: сверяем сеть и не даём уйти в минус (underflow → `Overflow`).
    pub fn checked_sub(&self, other: &Amount) -> Result<Amount, AmountError> {
        self.ensure_same_chain(other)?;
        let raw = self
            .raw
            .checked_sub(other.raw)
            .ok_or(AmountError::Overflow)?;
        Ok(Amount {
            chain: self.chain,
            raw,
        })
    }

    /// Общая проверка: обе суммы должны быть из одной сети.
    fn ensure_same_chain(&self, other: &Amount) -> Result<(), AmountError> {
        if self.chain != other.chain {
            return Err(AmountError::ChainMismatch {
                lhs: self.chain,
                rhs: other.chain,
            });
        }
        Ok(())
    }
}

/// Newtype-идентификаторы поверх `Uuid`. Отдельный тип на каждую сущность не даёт случайно
/// передать `WalletId` туда, где ждут `UserId` — компилятор ловит это за нас.
pub mod ids {
    use super::*;

    macro_rules! uuid_newtype {
        ($name:ident) => {
            #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
            pub struct $name(pub Uuid);

            impl $name {
                /// Сгенерировать новый случайный идентификатор (UUID v4).
                pub fn new() -> Self {
                    Self(Uuid::new_v4())
                }
            }

            impl Default for $name {
                fn default() -> Self {
                    Self::new()
                }
            }

            impl std::fmt::Display for $name {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    self.0.fmt(f)
                }
            }
        };
    }

    uuid_newtype!(UserId);
    uuid_newtype!(WalletId);
    uuid_newtype!(TransactionId);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eth(n: u64) -> Amount {
        Amount::new(Chain::Ethereum, U256::from(n))
    }

    #[test]
    fn chain_as_str_matches_db_values() {
        assert_eq!(Chain::Ethereum.as_str(), "ethereum");
        assert_eq!(Chain::Bitcoin.as_str(), "bitcoin");
        assert_eq!(Chain::Solana.as_str(), "solana");
    }

    #[test]
    fn only_approved_can_withdraw() {
        assert!(KycStatus::Approved.can_withdraw());
        assert!(!KycStatus::Pending.can_withdraw());
        assert!(!KycStatus::Rejected.can_withdraw());
    }

    #[test]
    fn checked_add_same_chain() {
        let sum = eth(2).checked_add(&eth(3)).unwrap();
        assert_eq!(sum, eth(5));
    }

    #[test]
    fn checked_add_rejects_chain_mismatch() {
        let a = eth(1);
        let b = Amount::new(Chain::Bitcoin, U256::from(1u64));
        assert_eq!(
            a.checked_add(&b),
            Err(AmountError::ChainMismatch {
                lhs: Chain::Ethereum,
                rhs: Chain::Bitcoin
            })
        );
    }

    #[test]
    fn checked_add_detects_overflow() {
        let max = Amount::new(Chain::Ethereum, U256::MAX);
        assert_eq!(max.checked_add(&eth(1)), Err(AmountError::Overflow));
    }

    #[test]
    fn checked_sub_underflow_is_overflow_error() {
        assert_eq!(eth(1).checked_sub(&eth(2)), Err(AmountError::Overflow));
    }

    #[test]
    fn checked_sub_rejects_chain_mismatch() {
        let a = eth(5);
        let b = Amount::new(Chain::Solana, U256::from(1u64));
        assert!(matches!(
            a.checked_sub(&b),
            Err(AmountError::ChainMismatch { .. })
        ));
    }
}
