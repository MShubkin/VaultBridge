//! Bitcoin 2-of-3 multisig (бонус): собираем redeem-скрипт и P2SH testnet-адрес из трёх
//! ключей. Смысл — показать мультиподпись: чтобы потратить средства, нужны любые два
//! ключа из трёх. Здесь только построение адреса; сама трата вне рамок демо.

use k256::ecdsa::VerifyingKey;

use crate::hd;

/// Опкод `OP_2` — порог «нужно 2 подписи».
const OP_2: u8 = 0x52;
/// Опкод `OP_3` — всего 3 ключа в наборе.
const OP_3: u8 = 0x53;
/// Опкод `OP_CHECKMULTISIG` — собственно проверка мультиподписи.
const OP_CHECKMULTISIG: u8 = 0xae;

/// Redeem-скрипт `OP_2 <pk1> <pk2> <pk3> OP_3 OP_CHECKMULTISIG` (сжатые ключи).
pub fn redeem_script_2of3(pubkeys: &[VerifyingKey; 3]) -> Vec<u8> {
    let mut script = Vec::with_capacity(105);
    script.push(OP_2);
    for vk in pubkeys {
        let point = vk.to_encoded_point(true); // 33 байта (compressed)
        let bytes = point.as_bytes();
        script.push(bytes.len() as u8); // push-opcode 0x21
        script.extend_from_slice(bytes);
    }
    script.push(OP_3);
    script.push(OP_CHECKMULTISIG);
    script
}

/// P2SH testnet-адрес скрипта: base58check(0xC4 || hash160(script)).
pub fn p2sh_testnet_address(script: &[u8]) -> String {
    let mut payload = Vec::with_capacity(21);
    payload.push(0xc4); // testnet script-hash version
    payload.extend_from_slice(&hd::hash160(script));
    bs58::encode(payload).with_check().into_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hd::{derive_secp256k1, seed_from_mnemonic};

    const M: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn three_pubkeys() -> [VerifyingKey; 3] {
        let seed = seed_from_mnemonic(M, "").unwrap();
        [
            *derive_secp256k1(&seed[..], "m/48'/1'/0'/0/0")
                .unwrap()
                .verifying_key(),
            *derive_secp256k1(&seed[..], "m/48'/1'/0'/0/1")
                .unwrap()
                .verifying_key(),
            *derive_secp256k1(&seed[..], "m/48'/1'/0'/0/2")
                .unwrap()
                .verifying_key(),
        ]
    }

    #[test]
    fn redeem_script_is_well_formed() {
        let script = redeem_script_2of3(&three_pubkeys());
        assert_eq!(script.len(), 1 + 3 * (1 + 33) + 1 + 1); // 105
        assert_eq!(script[0], OP_2);
        assert_eq!(*script.last().unwrap(), OP_CHECKMULTISIG);
    }

    #[test]
    fn p2sh_address_is_deterministic_and_testnet() {
        let script = redeem_script_2of3(&three_pubkeys());
        let a1 = p2sh_testnet_address(&script);
        let a2 = p2sh_testnet_address(&script);
        assert_eq!(a1, a2);
        // testnet P2SH-адреса начинаются с '2'; декодируется в 0xc4 || 20 байт.
        assert!(a1.starts_with('2'));
        let decoded = bs58::decode(&a1).with_check(None).into_vec().unwrap();
        assert_eq!(decoded.len(), 21);
        assert_eq!(decoded[0], 0xc4);
    }

    #[test]
    fn key_order_matters() {
        let [a, b, c] = three_pubkeys();
        let s1 = redeem_script_2of3(&[a, b, c]);
        let s2 = redeem_script_2of3(&[c, b, a]);
        // Разный порядок ключей → разный скрипт → разный адрес (как в реальном multisig).
        assert_ne!(p2sh_testnet_address(&s1), p2sh_testnet_address(&s2));
    }
}
