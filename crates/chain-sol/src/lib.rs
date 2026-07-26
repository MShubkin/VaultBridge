//! Solana-адаптер: лёгкий JSON-RPC клиент (devnet) + base58-валидация адресов.
//!
//! Полноценный spend: строим legacy-message системного `transfer`, подписываем
//! ВСЁ сообщение ed25519 (`SignScheme::Ed25519Message`) и собираем транзакцию вручную
//! (compact-u16 «shortvec» + Solana wire-формат) — без `solana-sdk`, в духе остального
//! тонкого клиента. Read-путь (валидация/баланс/broadcast) — реальный через RPC.

//Потенциальные улучшения:
//Добавить таймауты для RPC-запросов.
//Retry-логику при ошибках сети.
//Кеширование blockhash (можно использовать запасной, если свежий недоступен).
//Поддержка Versioned-транзакций (TxV0) для композитных инструкций.

use core_domain::{Amount, Chain, U256};
use serde::Deserialize;

use blockchain::{
    BalanceView, BlockchainClient, ChainError, SignScheme, SignedTransaction, SigningRequest,
    TxObservation, UnsignedTransaction, WithdrawRequest,
};

/// Базовая комиссия Solana — 5000 лампортов за подпись.
const BASE_FEE_LAMPORTS: u64 = 5000;

/// System Program ID — 32 нулевых байта (base58 «1111…1»).
const SYSTEM_PROGRAM_ID: [u8; 32] = [0u8; 32];

/// Декодировать base58-pubkey ровно в 32 байта.
fn decode_pubkey(address: &str) -> Result<[u8; 32], ChainError> {
    let bytes = bs58::decode(address)
        .into_vec()
        .map_err(|_| ChainError::InvalidAddress(Chain::Solana))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| ChainError::InvalidAddress(Chain::Solana))?;
    Ok(arr)
}

/// Solana использует вариативное кодирование длин массивов для экономии места
/// Примеры:
// len = 1 → [0x01] (1 байт)
// len = 127 → [0x7F] (1 байт)
// len = 128 → [0x80, 0x01] (2 байта: 128 = 0x80 | 0x01)
// Это позволяет кодировать числа до 16 бит (макс. 16383) в 1-2 байта
fn encode_length(mut len: usize, out: &mut Vec<u8>) {
    loop {
        let mut byte = (len & 0x7f) as u8;
        len >>= 7;
        if len != 0 {
            byte |= 0x80; // Устанавливаем старший бит → "продолжай читать"
        }
        out.push(byte);
        if len == 0 {
            break;
        }
    }
}

/// Сериализованный legacy-message для системного `transfer` (1 подписант = плательщик).
///
/// Порядок ключей по правилу Solana (подписанты → writable → readonly): `[from, to,
/// system_program]`; заголовок `(1, 0, 1)`. Инструкция Transfer: дискриминант u32 LE = 2,
/// затем lamports u64 LE.
fn build_transfer_message(
    from: &[u8; 32],
    to: &[u8; 32],
    lamports: u64,
    blockhash: &[u8; 32],
) -> Vec<u8> {
    let mut out = Vec::new();
    // Header(3 байта): num_required_signatures, num_readonly_signed, num_readonly_unsigned.
    // 1 подписант (from)
    // 0 подписантов только для чтения
    // 1 неподписанный readonly (system_program)
    out.extend_from_slice(&[1, 0, 1]);
    // account_keys(3 акаунта): from (signer/writable), to (writable), system_program (readonly).
    encode_length(3, &mut out);
    out.extend_from_slice(from); //index 0: подписант, writable
    out.extend_from_slice(to);
    out.extend_from_slice(&SYSTEM_PROGRAM_ID);
    // recent_blockhash(32 байта)
    out.extend_from_slice(blockhash);
    // instructions: одна.
    encode_length(1, &mut out);
    out.push(2); // program_id_index → system_program
    // Аккаунты, участвующие в инструкции
    encode_length(2, &mut out); // // 2 аккаунта: account indices: from, to
    out.extend_from_slice(&[0u8, 1u8]);
    // Данные инструкции (SystemInstruction::Transfer)
    let mut data = vec![2u8, 0, 0, 0]; // u32 LE = 2 (дискриминант). SystemInstruction::Transfer имеет дискриминант 2
    data.extend_from_slice(&lamports.to_le_bytes()); // сумма в LE(lamports)
    encode_length(data.len(), &mut out);
    out.extend_from_slice(&data);
    out
}

/// Solana-адаптер: тонкий JSON-RPC поверх HTTP.
pub struct SolClient {
    /// URL JSON-RPC узла (devnet).
    rpc_url: String,
    /// Переиспользуемый HTTP-клиент.
    http: reqwest::Client,
    /// Параметры сети.
    config: blockchain::ChainConfig,
}

/// Обёртка ответа JSON-RPC: ровно одно из полей заполнено — `result` при успехе или
/// `error` при ошибке.
#[derive(Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<serde_json::Value>,
}

/// Ответ `getBalance`: баланс в лампортах.
#[derive(Deserialize)]
struct BalanceValue {
    value: u64,
}

/// Ответ `getLatestBlockhash`.
#[derive(Deserialize)]
struct LatestBlockhash {
    value: BlockhashInner,
}

/// Вложенный blockhash в ответе `getLatestBlockhash` (base58-строка).
#[derive(Deserialize)]
struct BlockhashInner {
    blockhash: String,
}

/// Ответ `getSignatureStatuses`: по элементу на каждую запрошенную подпись.
#[derive(Deserialize)]
struct SignatureStatuses {
    value: Vec<Option<SignatureStatus>>,
}

#[derive(Deserialize)]
struct SignatureStatus {
    /// `null`, когда транзакция укоренена (finalized) — максимум подтверждений.
    confirmations: Option<u64>,
    /// Непустое значение — транзакция исполнилась с ошибкой (провал).
    err: Option<serde_json::Value>,
}

/// Ответ `isBlockhashValid`: жив ли ещё recent blockhash (не протух ли).
#[derive(Deserialize)]
struct BlockhashValid {
    value: bool,
}

impl SolClient {
    /// Создать адаптер по URL RPC-узла. Соединение ленивое — поднимается при первом запросе.
    pub fn new(rpc_url: &str, config: blockchain::ChainConfig) -> Self {
        Self {
            rpc_url: rpc_url.to_string(),
            http: reqwest::Client::new(),
            config,
        }
    }

    /// Адрес Solana — base58, ровно 32 байта (ed25519 pubkey).
    fn validate_pubkey(&self, address: &str) -> Result<(), ChainError> {
        match bs58::decode(address).into_vec() {
            Ok(bytes) if bytes.len() == 32 => Ok(()),
            _ => Err(ChainError::InvalidAddress(Chain::Solana)),
        }
    }

    /// Один JSON-RPC вызов. Собирает конверт `{jsonrpc, id, method, params}`, отправляет POST
    /// и разбирает ответ: поле `error` превращается в `ChainError`, пустой `result` — тоже.
    async fn rpc<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T, ChainError> {
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": method, "params": params
        });
        let resp: RpcResponse<T> = self
            .http
            .post(&self.rpc_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?
            .json()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        if let Some(err) = resp.error {
            return Err(ChainError::Rpc(format!("rpc: {err}")));
        }
        resp.result
            .ok_or_else(|| ChainError::Rpc("empty result".into()))
    }
}

#[async_trait::async_trait]
impl BlockchainClient for SolClient {
    fn chain(&self) -> Chain {
        Chain::Solana
    }

    fn config(&self) -> &blockchain::ChainConfig {
        &self.config
    }

    fn validate_address(&self, address: &str) -> Result<(), ChainError> {
        self.validate_pubkey(address)
    }

    async fn get_balance(&self, address: &str) -> Result<BalanceView, ChainError> {
        self.validate_pubkey(address)?;
        let bal: BalanceValue = self.rpc("getBalance", serde_json::json!([address])).await?;
        let lamports = U256::from(bal.value);
        // На Solana есть rent-exemption, но для демо-баланса reserved=0 (read-путь).
        Ok(BalanceView {
            total: Amount::new(Chain::Solana, lamports),
            reserved: Amount::new(Chain::Solana, U256::ZERO),
            spendable: Amount::new(Chain::Solana, lamports),
        })
    }

    async fn estimate_fee(&self, _req: &WithdrawRequest) -> Result<Amount, ChainError> {
        // Простой перевод — одна подпись.
        Ok(Amount::new(Chain::Solana, U256::from(BASE_FEE_LAMPORTS)))
    }

    //Сборка неподписанной транзакции
    async fn build_unsigned(
        &self,
        req: &WithdrawRequest,
        _fee: &Amount,
    ) -> Result<UnsignedTransaction, ChainError> {
        // Комиссия на Solana платится отдельно плательщиком (from), не входит в transfer.
        let from = decode_pubkey(&req.from_address)?;
        let to = decode_pubkey(&req.to_address)?;
        let lamports = u64::try_from(req.amount.raw)
            .map_err(|_| ChainError::Rpc("amount exceeds u64 lamports".into()))?;

        // recent_blockhash — обязателен и быстро протухает (~150 блоков), берём свежий.
        let bh: LatestBlockhash = self
            .rpc(
                "getLatestBlockhash",
                serde_json::json!([{ "commitment": "finalized" }]),
            )
            .await?;
        let blockhash = decode_pubkey(&bh.value.blockhash)
            .map_err(|_| ChainError::Rpc("bad blockhash".into()))?;

        // Строим message
        let message = build_transfer_message(&from, &to, lamports, &blockhash);
        // Account-модель Solana: один подписант (плательщик) подписывает ВСЁ сообщение.
        Ok(UnsignedTransaction {
            chain: Chain::Solana,
            requests: vec![SigningRequest {
                scheme: SignScheme::Ed25519Message,
                derivation_path: req.derivation_path.clone(),
                payload: message.clone(),
            }],
            context: message,
            // Сохраняем recent blockhash: по нему реконсилятор отличит «истекла» от «не дошла».
            tracking: Some(bh.value.blockhash.clone()),
        })
    }

    //Сборка подписанной транзакции
    //Итоговый wire-формат:[shortvec_len][sig1][sig2]...[message]
    fn assemble_signed(
        &self,
        unsigned: &UnsignedTransaction,
        signatures: &[Vec<u8>],
    ) -> Result<SignedTransaction, ChainError> {
        if signatures.len() != unsigned.requests.len() {
            return Err(ChainError::Rpc(format!(
                "expected {} signatures, got {}",
                unsigned.requests.len(),
                signatures.len()
            )));
        }
        // Транзакция = compact-массив подписей (по 64 байта, ed25519) ++ message.
        let mut raw = Vec::new();
        //Compact-массив подписей
        encode_length(signatures.len(), &mut raw);
        for sig in signatures {
            if sig.len() != 64 {
                return Err(ChainError::Rpc("ed25519 signature must be 64 bytes".into()));
            }
            raw.extend_from_slice(sig);
        }
        // Само сообщение (без изменений)
        raw.extend_from_slice(&unsigned.context);
        Ok(SignedTransaction {
            chain: Chain::Solana,
            raw,
        })
    }

    fn txid(&self, signed: &SignedTransaction) -> Result<String, ChainError> {
        // На Solana идентификатор транзакции — её первая подпись (base58). В нашей сборке
        // raw = compact_len(1) ++ signature(64) ++ message, поэтому подпись = raw[1..65].
        if signed.raw.len() < 65 {
            return Err(ChainError::Rpc("signed tx too short".into()));
        }
        Ok(bs58::encode(&signed.raw[1..65]).into_string())
    }

    async fn broadcast(&self, signed: &SignedTransaction) -> Result<String, ChainError> {
        // sendTransaction принимает base64-сериализованную подписанную транзакцию.
        let b64 = base64_encode(&signed.raw);
        let sig: String = self
            .rpc(
                "sendTransaction",
                serde_json::json!([b64, { "encoding": "base64" }]),
            )
            .await?;
        Ok(sig)
    }
    //Проверка статуса
    async fn tx_status(
        &self,
        tx_hash: &str,
        _from_address: &str,
        tracking: Option<&str>,
    ) -> Result<TxObservation, ChainError> {
        // getSignatureStatuses: value[0] = null (не найдена) или { confirmations, err }.
        let statuses: SignatureStatuses = self
            .rpc(
                "getSignatureStatuses",
                serde_json::json!([[tx_hash], { "searchTransactionHistory": true }]),
            )
            .await?;
        match statuses.value.into_iter().next().flatten() {
            // err непустой → транзакция исполнилась с ошибкой.
            Some(s) if s.err.is_some() => Ok(TxObservation::Failed),
            // confirmations=null → укоренена (finalized): отдаём максимум.
            Some(s) => Ok(TxObservation::Pending {
                confirmations: s.confirmations.unwrap_or(u64::MAX),
            }),
            // Не найдена. Если знаем исходный blockhash и он больше не валиден — транзакция
            // уже не попадёт в блок: это Expired. Иначе она ещё может дойти → NotFound.
            None => {
                if let Some(blockhash) = tracking {
                    let valid: BlockhashValid = self
                        .rpc(
                            "isBlockhashValid",
                            serde_json::json!([blockhash, { "commitment": "finalized" }]),
                        )
                        .await?;
                    if !valid.value {
                        return Ok(TxObservation::Expired); // Уже не попадёт в блок
                    }
                }
                Ok(TxObservation::NotFound)
            }
        }
    }
}

/// Минимальный base64 (без внешней зависимости) для sendTransaction.
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}


#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> blockchain::ChainConfig {
        blockchain::ChainConfig {
            chain: Chain::Solana,
            decimals: 9,
            confirmations: None, // у Solana финальность по commitment, а не по числу блоков
            reorg_window: 0,
            dust_limit: U256::from(1u64),
            fee_cap_factor: 2.0,
        }
    }

    #[test]
    fn validates_base58_pubkey() {
        let c = SolClient::new("https://api.devnet.solana.com", cfg());
        // Системная программа Solana — валидный 32-байтовый pubkey.
        assert!(c
            .validate_address("11111111111111111111111111111111")
            .is_ok());
        assert!(c.validate_address("not-base58-!!!").is_err());
        assert!(c.validate_address("abc").is_err()); // короткий
    }

    #[test]
    fn base64_matches_known_vector() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn shortvec_matches_known_vectors() {
        let enc = |n| {
            let mut v = Vec::new();
            encode_length(n, &mut v);
            v
        };
        assert_eq!(enc(0), vec![0x00]);
        assert_eq!(enc(1), vec![0x01]);
        assert_eq!(enc(127), vec![0x7f]);
        assert_eq!(enc(128), vec![0x80, 0x01]);
        assert_eq!(enc(16384), vec![0x80, 0x80, 0x01]);
    }

    #[test]
    fn transfer_message_layout_is_correct() {
        let from = [0x11u8; 32];
        let to = [0x22u8; 32];
        let blockhash = [0x33u8; 32];
        let lamports = 1_000_000u64;
        let msg = build_transfer_message(&from, &to, lamports, &blockhash);

        // header(3) + acct_len(1) + keys(96) + blockhash(32) + ix_len(1)
        // + program_idx(1) + acct_idx_len(1) + acct_idx(2) + data_len(1) + data(12) = 150.
        assert_eq!(msg.len(), 150);
        // Заголовок: 1 подписант, 0 readonly-signed, 1 readonly-unsigned; затем 3 ключа.
        assert_eq!(&msg[0..4], &[1, 0, 1, 3]);
        assert_eq!(&msg[4..36], &from);
        assert_eq!(&msg[36..68], &to);
        assert_eq!(&msg[68..100], &SYSTEM_PROGRAM_ID);
        assert_eq!(&msg[100..132], &blockhash);
        // 1 инструкция; program_id_index=2; 2 аккаунта [0,1]; data_len=12.
        assert_eq!(&msg[132..138], &[1, 2, 2, 0, 1, 12]);
        // Transfer-дискриминант (u32 LE = 2) + lamports (u64 LE).
        assert_eq!(&msg[138..142], &[2, 0, 0, 0]);
        assert_eq!(&msg[142..150], &lamports.to_le_bytes());
    }

    #[test]
    fn assemble_prepends_signature_and_validates() {
        let c = SolClient::new("https://api.devnet.solana.com", cfg());
        let message = vec![0xABu8; 150];
        let unsigned = UnsignedTransaction {
            chain: Chain::Solana,
            requests: vec![SigningRequest {
                scheme: SignScheme::Ed25519Message,
                derivation_path: "m/44'/501'/0'/0'".into(),
                payload: message.clone(),
            }],
            context: message.clone(),
            tracking: None,
        };

        // Неверное число подписей и неверная длина — отказ.
        assert!(c.assemble_signed(&unsigned, &[]).is_err());
        assert!(c.assemble_signed(&unsigned, &[vec![0u8; 10]]).is_err());

        let sig = vec![0x44u8; 64];
        let signed = c
            .assemble_signed(&unsigned, std::slice::from_ref(&sig))
            .unwrap();
        // compact-len(1) ++ signature(64) ++ message.
        assert_eq!(signed.raw[0], 1);
        assert_eq!(&signed.raw[1..65], &sig[..]);
        assert_eq!(&signed.raw[65..], &message[..]);
        assert_eq!(signed.raw.len(), 1 + 64 + message.len());
    }
}
