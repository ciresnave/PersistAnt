#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# Guest side of the disk-full (ENOSPC) check for PostgreSQL and MySQL. run.ps1 copies this into the
# guest at run time (the base image is built once by a person, so nothing here may need a rebuild).
#
#   pa-small-db pg|mysql on SIZE_MB   a fresh ext4 volume of SIZE_MB, mounted OVER the server's own data
#                                     directory (so the distribution's service, config and AppArmor
#                                     profile apply unchanged), a fresh cluster in it, role pa, database
#                                     "full". A BALLAST file is left in the volume.
#   pa-small-db pg|mysql free         delete the ballast (what an operator does after a disk-full alarm)
#   pa-small-db pg|mysql off          stop, unmount, delete the volume, start the normal server again
#
# Test-only credentials, as in guest-user-data.yaml. Run as root.
set -eu

kind="$1"
op="$2"
img=/var/lib/pa-small-db.img
ballast_name=ballast

case "$kind" in
  pg)
    pgv=$(ls /usr/lib/postgresql | sort -V | tail -1)
    datadir=/var/lib/postgresql/$pgv/main
    owner=postgres
    stop() { pg_ctlcluster "$pgv" main stop -m immediate 2>/dev/null || true; }
    start() { pg_ctlcluster "$pgv" main start; }
    ready() { pg_isready -q -h 127.0.0.1; }
    ;;
  mysql)
    datadir=/var/lib/mysql
    owner=mysql
    stop() { service mysql stop >/dev/null 2>&1 || true; }
    start() { service mysql start; }
    ready() { mysqladmin --protocol=tcp -h 127.0.0.1 -upa -ppa-test-only ping --silent 2>/dev/null \
      || mysqladmin --protocol=socket -uroot ping --silent 2>/dev/null; }
    ;;
  *) echo "usage: pa-small-db pg|mysql on SIZE_MB | free | off" >&2; exit 2 ;;
esac

wait_ready() {
  i=0
  while [ "$i" -lt 120 ]; do
    if ready; then return 0; fi
    i=$((i + 1)); sleep 1
  done
  echo "$kind did not become ready" >&2
  return 1
}

case "$op" in
  on)
    size="$3"
    stop
    umount "$datadir" 2>/dev/null || true
    rm -f "$img"
    dd if=/dev/zero of="$img" bs=1M count="$size" status=none
    mkfs.ext4 -q -F "$img"
    # Keep the original data directory (and its contents) underneath, for "off".
    mount -o loop "$img" "$datadir"
    rm -rf "$datadir/lost+found"
    chown "$owner:$owner" "$datadir"
    chmod 700 "$datadir"
    if [ "$kind" = pg ]; then
      # 1 MB WAL segments: the default 16 MB segments alone would not leave a small volume any room.
      su postgres -c "/usr/lib/postgresql/$pgv/bin/initdb -D $datadir --wal-segsize=1 -A trust >/dev/null"
      start
      wait_ready
      su postgres -c "psql -qc \"CREATE ROLE pa SUPERUSER LOGIN PASSWORD 'pa-test-only'\" -c 'CREATE DATABASE \"full\"'"
    else
      # The redo log (innodb_redo_log_capacity) is 100 MB by default; 8 MB is the minimum.
      printf '[mysqld]\ninnodb_redo_log_capacity=8388608\n' > /etc/mysql/mysql.conf.d/zz-pa-small.cnf
      mysqld --initialize-insecure --user=mysql >/dev/null 2>&1
      start
      wait_ready
      mysql -uroot -e "CREATE USER IF NOT EXISTS 'pa'@'127.0.0.1' IDENTIFIED BY 'pa-test-only'; GRANT ALL ON *.* TO 'pa'@'127.0.0.1' WITH GRANT OPTION; CREATE DATABASE \`full\`;"
    fi
    sync
    # What is left after the cluster exists is the room the test fills; keep 2 MB aside as ballast.
    dd if=/dev/zero of="$datadir/$ballast_name" bs=1M count=2 status=none
    chown "$owner:$owner" "$datadir/$ballast_name"
    sync
    df -k "$datadir" | tail -1
    ;;
  free)
    rm -f "$datadir/$ballast_name"
    sync
    ;;
  off)
    stop
    umount "$datadir" 2>/dev/null || true
    rm -f "$img" /etc/mysql/mysql.conf.d/zz-pa-small.cnf
    start || true
    ;;
  *) echo "unknown op $op" >&2; exit 2 ;;
esac
