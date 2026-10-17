#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Start two throwaway MySQL 8.4 servers for crates/persistant-sqlx/tests/tls_mysql.rs:
# one with TLS on (MySQL 8.4 generates a private CA and a server certificate in its data directory
# at first start) and one with --tls-version=.
#
#   bash .github/scripts/mysql-tls.sh <tls-name> <tls-port> <plain-name> <plain-port> <ca-out.pem>
#
# The generated server certificate is signed by that CA but names no host we connect to, so the
# tests expect VerifyCa to accept it with the CA as root and VerifyFull to refuse it.
# The image is the same digest-pinned MySQL 8.4 the service container uses.
set -euo pipefail

tls_name=${1:?tls container name}
tls_port=${2:?tls host port}
plain_name=${3:?plain container name}
plain_port=${4:?plain host port}
ca_out=${5:?path for the CA certificate (PEM)}
image=mysql@sha256:24989179f58856db763f418010c1169fecc944e3ec1ea7e921572d83eb070f70

start() { # name port extra-server-args...
    local name=$1 port=$2
    shift 2
    docker rm -f "$name" >/dev/null 2>&1 || true
    docker run -d --name "$name" \
        -e MYSQL_ROOT_PASSWORD=persistant-test -e MYSQL_DATABASE=persistant \
        -p "127.0.0.1:${port}:3306" "$image" "$@" >/dev/null
}
# The image's init phase runs a temporary server on a socket only; the final one listens on TCP.
wait_for() { # name
    for _ in $(seq 1 90); do
        if docker exec "$1" mysql --get-server-public-key -h127.0.0.1 -uroot -ppersistant-test -N -e 'select 1' persistant >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    echo "mysql-tls: $1 did not come up" >&2
    docker logs "$1" >&2 || true
    exit 1
}

start "$tls_name" "$tls_port"
start "$plain_name" "$plain_port" --tls-version=
wait_for "$tls_name"
wait_for "$plain_name"
docker cp "$tls_name:/var/lib/mysql/ca.pem" "$ca_out"
echo "mysql-tls: $tls_name on 127.0.0.1:${tls_port} (TLS), $plain_name on 127.0.0.1:${plain_port} (no TLS), CA at $ca_out"
