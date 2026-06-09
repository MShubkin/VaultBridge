// Контрактные типы API — зеркало доменных типов бэкенда.
// Пока выверены вручную против OpenAPI (`ui/openapi.json`).
// Цель — заменить на сгенерированные: `npm run codegen` (openapi-typescript → schema.d.ts).

export type Role = "user" | "operator";
export type KycStatus = "pending" | "approved" | "rejected";
export type Chain = "ethereum" | "bitcoin" | "solana";
/// Машина состояний транзакции — зеркало core-domain::TxStatus.
export type TxStatus =
  | "created"
  | "signing"
  | "broadcast"
  | "unconfirmed"
  | "confirmed"
  | "failed"
  | "expired"
  | "replaced";

export interface LoginRequest {
  email: string;
  password: string;
}
export interface LoginResponse {
  access_token: string;
  expires_in: number;
}

export interface CreateUserRequest {
  email: string;
  password: string;
}
export interface UserProfile {
  id: string;
  email: string;
  kyc_status: KycStatus;
  role: Role;
}

export interface CreateWalletRequest {
  chain: Chain;
}
export interface WalletDto {
  id: string;
  chain: Chain;
  address: string;
  derivation_path: string;
  created_at_unix: number;
}

export interface WithdrawRequest {
  to_address: string;
  amount_raw: string;
  max_fee_raw?: string;
}
export interface QuoteResponse {
  estimated_fee_raw: string;
  max_fee_raw: string;
  total_debit_raw: string;
  spendable_raw: string;
}
export interface WithdrawResponse {
  tx_id: string;
  status: string;
  tx_hash: string | null;
  fee_raw: string;
}

// Операторский API.
export interface OpsTx {
  id: string;
  wallet_id: string;
  chain: Chain;
  to_address: string | null;
  amount_raw: string;
  status: TxStatus;
  tx_hash: string | null;
  created_at_unix: number;
}
export interface OpsAuditEntry {
  id: number;
  actor: string | null;
  action: string;
  wallet_id: string | null;
  result: string;
  created_at_unix: number;
}

/// Знаков после запятой по сети (для перевода human → raw); зеркало ChainConfig.decimals.
export const CHAIN_DECIMALS: Record<Chain, number> = {
  ethereum: 18,
  bitcoin: 8,
  solana: 9,
};
