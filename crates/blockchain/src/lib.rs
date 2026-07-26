//! Абстракция над блокчейнами: единый `trait BlockchainClient` и сопутствующие типы.
//!
//! Идея простая — вся бизнес-логика (сага вывода, выдача баланса, реконсиляция) работает
//! против одного трейта и не знает, EVM это, Bitcoin или Solana. Конкретику прячут
//! адаптеры в отдельных крейтах (`chain-evm`, `chain-btc`, `chain-sol`), а здесь лежит
//! только контракт, конфиг сети и детерминированный тест-двойник [`mock::MockChain`].
//!
//! Сетевые вызовы адаптеров тестируются отдельно (нужен реальный RPC), а сагу гоняем
//! против мока — поэтому логика проверяется без выхода в сеть.

use core_domain::{Amount, Chain, U256};

/// Детерминированный тест-двойник — только для тестов (см. фичу `testing`).
#[cfg(any(test, feature = "testing"))]
pub mod mock;
#[cfg(any(test, feature = "testing"))]
pub use mock::MockChain;

/// Ошибки уровня адаптера сети. Намеренно узкий набор: всё, что наружу, маппится в
/// доменные ошибки без утечки внутренних деталей RPC.
#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    /// Адрес не проходит валидацию формата или принадлежит не той сети.
    #[error("invalid address for {0}")]
    InvalidAddress(Chain),
    /// Сумма меньше dust-лимита сети — такой выход сеть не примет.
    #[error("amount below dust limit")]
    BelowDust,
    /// На адресе/входах не хватает средств на сумму + комиссию.
    #[error("insufficient funds")]
    InsufficientFunds,
    /// Любая ошибка обращения к ноде/RPC (текст — для логов, не для клиента).
    #[error("rpc error: {0}")]
    Rpc(String),
    /// Операция не поддержана для данной сети.
    #[error("chain not supported: {0}")]
    Unsupported(Chain),
}

/// Параметры сети вынесены в данные, а не в `match` по `Chain`. Так различия между
/// сетями (число подтверждений, dust, decimals) живут в одном месте, и добавление сети
/// не требует править ветвления по всему коду.
#[derive(Clone, Debug)]
pub struct ChainConfig {
    /// Сеть, к которой относится конфиг.
    pub chain: Chain,
    /// Сколько знаков после запятой в «человеческой» единице (18 у ETH, 8 у BTC, 9 у SOL).
    pub decimals: u8,
    /// Порог подтверждений для финализации (EVM/BTC). У Solana — `None`: там финальность
    /// определяется commitment'ом, а не числом блоков.
    pub confirmations: Option<u32>,
    /// Глубина возможного реорга — на сколько блоков назад статус ещё может откатиться.
    pub reorg_window: u32,
    /// Минимальная сумма выхода: всё, что ниже, считается пылью и отклоняется.
    pub dust_limit: U256,
    /// Во сколько раз поднять оценку комиссии до потолка `max_fee`, если клиент не задал
    /// его явно. Защита от проскальзывания комиссии между оценкой и отправкой.
    pub fee_cap_factor: f64,
}

/// Баланс с разбивкой на «всего» и «доступно». Разница — неснижаемый резерв: у Solana это
/// rent-exemption, у EVM/BTC резерва нет и `reserved` равен нулю.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalanceView {
    /// Полный баланс адреса.
    pub total: Amount,
    /// Часть, которую нельзя тратить (rent-exemption и т.п.).
    pub reserved: Amount,
    /// Сколько реально доступно к выводу (`total - reserved`).
    pub spendable: Amount,
}

/// Запрос на вывод в доменных терминах — без транспортных и сетевых деталей.
#[derive(Clone, Debug)]
pub struct WithdrawRequest {
    /// Сеть, в которой делается перевод.
    pub chain: Chain,
    /// Адрес-источник.
    pub from_address: String,
    /// Адрес-получатель.
    pub to_address: String,
    /// Сумма перевода.
    pub amount: Amount,
    /// Путь HD-деривации ключа кошелька — по нему signing-service адресует подпись,
    /// не получая сам приватный ключ.
    pub derivation_path: String,
}

/// Чем подписывать конкретный запрос. Разные сети ждут разного: EVM и каждый вход
/// Bitcoin — это ECDSA над 32-байтовым хэшем, а Solana подписывает целиком всё сообщение
/// ключом ed25519.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignScheme {
    /// secp256k1 ECDSA над 32-байтовым prehash (EVM и каждый вход Bitcoin).
    EcdsaPrehash,
    /// ed25519 над всем сообщением целиком (Solana).
    Ed25519Message,
}

/// Один запрос на подпись. У account-моделей он всегда один, у UTXO — по одному на каждый
/// вход транзакции.
#[derive(Clone, Debug)]
pub struct SigningRequest {
    /// Какой схемой подписывать этот payload.
    pub scheme: SignScheme,
    /// Путь деривации ключа, которым подписывать.
    pub derivation_path: String,
    /// Что именно подписывать: 32-байтовый prehash для `EcdsaPrehash` либо всё сообщение
    /// для `Ed25519Message`.
    pub payload: Vec<u8>,
}

/// Несформированная транзакция: список запросов на подпись (один у account-моделей,
/// несколько у UTXO) плюс непрозрачный контекст, по которому адаптер потом соберёт raw.
#[derive(Clone, Debug)]
pub struct UnsignedTransaction {
    /// Сеть транзакции.
    pub chain: Chain,
    /// Запросы на подпись — по одному на вход/подписанта, в строгом порядке.
    pub requests: Vec<SigningRequest>,
    /// Контекст сборки: сериализованная заготовка транзакции и метаданные входов.
    /// Понятен только адаптеру, который его создал.
    pub context: Vec<u8>,
    /// Компактный chain-specific токен, который стоит сохранить рядом с транзакцией для
    /// последующей реконсиляции: EVM — `nonce`, Solana — recent blockhash, Bitcoin — `None`.
    /// По нему адаптер потом отличает «заменена» (EVM) и «истекла» (Solana) от «ещё не дошла».
    pub tracking: Option<String>,
}

/// Подписанная транзакция в виде готовых к отправке байт.
#[derive(Clone, Debug)]
pub struct SignedTransaction {
    /// Сеть транзакции.
    pub chain: Chain,
    /// Сырые байты подписанной транзакции (формат — по сети).
    pub raw: Vec<u8>,
}

/// Что реконсилятор видит про транзакцию в сети. Богаче, чем просто число подтверждений:
/// позволяет двигать FSM не только вперёд (в `Confirmed`), но и в терминальные неуспехи
/// (`Failed`/`Expired`/`Replaced`), а также откатывать обратно при реорге.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxObservation {
    /// Транзакция не видна в сети — ни в блоке, ни в мемпуле. Может ещё появиться.
    NotFound,
    /// Видна, но не финализирована: `confirmations` подтверждений (`0` — в мемпуле/не в блоке).
    /// Решение «достаточно ли» принимает сканер по порогу из `ChainConfig`.
    Pending { confirmations: u64 },
    /// Окончательно отклонена сетью (например, EVM-receipt со `status = 0`).
    Failed,
    /// Истекла, не попав в блок (Solana: recent blockhash больше не валиден).
    Expired,
    /// Заменена другой транзакцией с тем же nonce (EVM: bump gas / RBF).
    Replaced,
}

/// Контракт адаптера сети. Делит работу на read-путь (валидация, баланс, комиссия,
/// подтверждения) и write-путь (собрать → подписать → отправить). Подпись вынесена наружу:
/// адаптер строит транзакцию и собирает её обратно, но ключей не держит.
#[async_trait::async_trait]
pub trait BlockchainClient: Send + Sync {
    /// Какую сеть обслуживает адаптер.
    fn chain(&self) -> Chain;
    /// Конфиг этой сети.
    fn config(&self) -> &ChainConfig;

    /// Проверить формат и принадлежность адреса нужной сети. Делается до AML и подписи,
    /// чтобы заведомо битый адрес не уходил дальше по пайплайну.
    fn validate_address(&self, address: &str) -> Result<(), ChainError>;

    /// Текущий баланс адреса с разбивкой total/reserved/spendable.
    async fn get_balance(&self, address: &str) -> Result<BalanceView, ChainError>;
    /// Оценить комиссию за перевод.
    async fn estimate_fee(&self, req: &WithdrawRequest) -> Result<Amount, ChainError>;

    /// Собрать несформированную транзакцию. Всё, что зависит от состояния сети
    /// (nonce, выбор UTXO, свежий blockhash), выбирается именно здесь.
    async fn build_unsigned(
        &self,
        req: &WithdrawRequest,
        fee: &Amount,
    ) -> Result<UnsignedTransaction, ChainError>;

    /// Прикрепить подписи к заготовке и получить готовый raw. Подписи идут в том же
    /// порядке, что и `requests`: у account-моделей одна, у UTXO — по одной на вход.
    fn assemble_signed(
        &self,
        unsigned: &UnsignedTransaction,
        signatures: &[Vec<u8>],
    ) -> Result<SignedTransaction, ChainError>;

    /// Детерминированный идентификатор подписанной транзакции, вычислимый **до** отправки
    /// (для всех наших сетей он однозначно выводится из подписанных байт). Сага сохраняет
    /// его вместе со статусом `Broadcast` ещё до `broadcast()`, чтобы краш во время отправки
    /// не оставил запись без `tx_hash`: реконсилятор увидит её и досверит статус, а
    /// повторная отправка безопасна за счёт идемпотентности `broadcast`.
    fn txid(&self, signed: &SignedTransaction) -> Result<String, ChainError>;

    /// Отправить транзакцию в сеть и вернуть её hash/подпись. За идемпотентность отвечает
    /// сама сеть (повторный broadcast того же raw) и сага.
    async fn broadcast(&self, signed: &SignedTransaction) -> Result<String, ChainError>;

    /// Наблюдаемое состояние транзакции в сети — для фонового реконсилятора. Помимо hash
    /// принимает `from_address` (адрес-отправитель) и `tracking` (см. [`UnsignedTransaction`]):
    /// они нужны, чтобы отличить «заменена»/«истекла» от «ещё не дошла». Например, EVM по
    /// `from_address` + nonce проверяет, не занял ли nonce другой транзакцией; Solana по
    /// blockhash — не протух ли он. Дефолт — `NotFound`: адаптер без поддержки не двигает статус.
    async fn tx_status(
        &self,
        _tx_hash: &str,
        _from_address: &str,
        _tracking: Option<&str>,
    ) -> Result<TxObservation, ChainError> {
        Ok(TxObservation::NotFound)
    }
}
