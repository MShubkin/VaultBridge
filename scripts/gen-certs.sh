#!/usr/bin/env bash
# Генерация демо-сертификатов для взаимного TLS gateway↔signing (spec.md §8.4).
#
# Делает CA и две пары (server/client), все подписаны CA. Сервер требует клиентский
# сертификат, клиент проверяет сервер по CA. Сертификаты НЕ коммитим (см. .gitignore) —
# в проде их выдаёт внутренний CA / cert-manager / mesh (SPIFFE).
#
# Использование:
#   ./scripts/gen-certs.sh [out_dir] [server_cn]
# По умолчанию: out_dir=certs, server_cn=localhost
set -euo pipefail

OUT="${1:-certs}"
CN="${2:-localhost}"
DAYS=365
mkdir -p "$OUT"
cd "$OUT"

# 1. CA
openssl genrsa -out ca.key 4096 >/dev/null 2>&1
openssl req -x509 -new -nodes -key ca.key -sha256 -days "$DAYS" \
  -subj "/CN=VaultBridge Demo CA" -out ca.crt >/dev/null 2>&1

# Хелпер: выпустить сертификат, подписанный CA, с заданным EKU и SAN.
issue() {
  local name="$1" cn="$2" eku="$3" san="$4"
  openssl genrsa -out "$name.key" 2048 >/dev/null 2>&1
  openssl req -new -key "$name.key" -subj "/CN=$cn" -out "$name.csr" >/dev/null 2>&1
  openssl x509 -req -in "$name.csr" -CA ca.crt -CAkey ca.key -CAcreateserial \
    -days "$DAYS" -sha256 -out "$name.crt" \
    -extfile <(printf "extendedKeyUsage=%s\nsubjectAltName=%s\n" "$eku" "$san") \
    >/dev/null 2>&1
  rm -f "$name.csr"
}

# 2. Server (serverAuth, SAN = CN)
issue server "$CN" "serverAuth" "DNS:$CN"
# 3. Client (clientAuth)
issue client "vaultbridge-gateway" "clientAuth" "DNS:vaultbridge-gateway"

rm -f ca.srl
echo "Сертификаты в $OUT/: ca.crt, server.{crt,key}, client.{crt,key}"
echo
echo "signing-service:"
echo "  SIGNER_TLS_CERT=$OUT/server.crt SIGNER_TLS_KEY=$OUT/server.key SIGNER_TLS_CLIENT_CA=$OUT/ca.crt"
echo "api-gateway:"
echo "  SIGNER_GRPC_ENDPOINT=https://$CN:50051 SIGNER_TLS_CLIENT_CERT=$OUT/client.crt \\"
echo "  SIGNER_TLS_CLIENT_KEY=$OUT/client.key SIGNER_TLS_CA=$OUT/ca.crt SIGNER_TLS_DOMAIN=$CN"
