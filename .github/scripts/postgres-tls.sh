#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Start a throwaway PostgreSQL 16 with TLS on, for crates/persistant-sqlx/tests/tls.rs.
#
#   bash .github/scripts/postgres-tls.sh <container-name> <host-port> <ca-out.pem>
#
# Makes a private CA and a leaf certificate for `localhost` / 127.0.0.1 inside the container (the
# leaf is CA:FALSE: a CA certificate presented as the server certificate is rejected by rustls
# with CaUsedAsEndEntity, which is itself a check we want the tests to rely on), starts the server
# with ssl=on, waits until the *final* server answers, and copies the CA certificate out.
# The image is the same digest-pinned PostgreSQL 16 the plain service container uses.
set -euo pipefail

name=${1:?container name}
port=${2:?host port}
ca_out=${3:?path for the CA certificate (PEM)}
image=postgres@sha256:ca0bd484cb98bf4b24eb1010e73fb3fcbd6714d240fbc1a10eea5b7dbecb641d

docker rm -f "$name" >/dev/null 2>&1 || true
MSYS_NO_PATHCONV=1 docker run -d --name "$name" \
    -e POSTGRES_PASSWORD=persistant-test -e POSTGRES_DB=persistant \
    -p "127.0.0.1:${port}:5432" --entrypoint bash "$image" -c '
set -e
cd /tmp
openssl req -new -x509 -nodes -days 2 -subj "/CN=persistant-test-ca" \
    -addext "basicConstraints=critical,CA:TRUE" -out ca.crt -keyout ca.key
openssl req -new -nodes -subj "/CN=localhost" -out server.csr -keyout server.key
printf "subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=CA:FALSE\n" > ext.cnf
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 2 \
    -extfile ext.cnf -out server.crt
chown postgres:postgres server.crt server.key
chmod 600 server.key
exec docker-entrypoint.sh postgres -c ssl=on -c ssl_cert_file=/tmp/server.crt -c ssl_key_file=/tmp/server.key
' >/dev/null

# The image's init phase runs a temporary server with ssl off; only the final one reports ssl=on.
ready=no
for _ in $(seq 1 90); do
    if docker exec "$name" psql -U postgres -d persistant -tAc 'show ssl' 2>/dev/null | grep -qx on; then
        ready=yes
        break
    fi
    sleep 1
done
if [ "$ready" != yes ]; then
    echo "postgres-tls: the server did not come up with ssl=on" >&2
    docker logs "$name" >&2 || true
    exit 1
fi
docker cp "$name:/tmp/ca.crt" "$ca_out"
echo "postgres-tls: $name listening on 127.0.0.1:${port}, CA certificate at $ca_out"
