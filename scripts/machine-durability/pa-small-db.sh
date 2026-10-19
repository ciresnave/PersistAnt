#!/bin/sh
# SPDX-License-Identifier: MIT OR Apache-2.0
# Guest side of the disk-full (ENOSPC) check for PostgreSQL and MySQL. run.ps1 copies this into the
# guest at run time (the base image is built once by a person, so nothing here may need a rebuild).
#
#   pa-small-db pg|mysql on SIZE_MB ROOM_MB
#                                     a fresh ext4 volume (no reserved blocks) of SIZE_MB, mounted OVER the
#                                     server's own data directory (so the distribution's service, config and
#                                     AppArmor profile apply unchanged), a fresh cluster in it, role pa,
#                                     database "full". An 8 MB BALLAST file, and a FILLER file that leaves
#                                     ROOM_MB free for the test to fill (the cluster alone needs ~35 MB for
#                                     PostgreSQL and ~110 MB for MySQL; filling takes minutes per 10 MB).
#   pa-small-db pg|mysql free         delete the ballast and start the server if the full disk stopped it
#   pa-small-db pg|mysql off          stop, unmount, delete the volume, start the normal server again
#
# Test-only credentials, as in guest-user-data.yaml. Run as root.
set -eu

kind="$1"
op="$2"
img=/var/lib/pa-small-db-$kind.img
step=start
# Say where it stopped: a failure here used to reach the host with no output at all.
trap 'rc=$?; [ "$rc" = 0 ] || echo "pa-small-db $kind $op: failed at step [$step] (exit $rc)" >&2' EXIT
note() { step="$1"; echo "pa-small-db $kind $op: $1" >&2; }
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
  *) echo "usage: pa-small-db pg|mysql on SIZE_MB ROOM_MB | free | off" >&2; exit 2 ;;
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
    room="$4"
    note "stop the server and clear anything left by an earlier run"
    stop
    umount -l "$datadir" 2>/dev/null || true
    rm -f "$img"
    note "make the volume"
    dd if=/dev/zero of="$img" bs=1M count="$size" status=none
    mkfs.ext4 -q -F -m 0 "$img"
    # Keep the original data directory (and its contents) underneath, for "off".
    note "mount over $datadir"
    mount -o loop "$img" "$datadir"
    rm -rf "$datadir/lost+found"
    chown "$owner:$owner" "$datadir"
    chmod 700 "$datadir"
    note "initialise a new cluster"
    if [ "$kind" = pg ]; then
      # 1 MB WAL segments: the default 16 MB segments alone would not leave a small volume any room.
      su postgres -c "/usr/lib/postgresql/$pgv/bin/initdb -D $datadir --wal-segsize=1 -A trust >/dev/null"
      note "start postgres"
      start
      wait_ready
      note "create role and database"
      su postgres -c "psql -qc \"CREATE ROLE pa SUPERUSER LOGIN PASSWORD 'pa-test-only'\" -c 'CREATE DATABASE \"full\"'"
    else
      # The redo log (innodb_redo_log_capacity) is 100 MB by default; 8 MB is the minimum.
      printf '[mysqld]\ninnodb_redo_log_capacity=8388608\n' > /etc/mysql/mysql.conf.d/zz-pa-small.cnf
      mysqld --initialize-insecure --user=mysql >&2
      note "start mysql"
      start
      wait_ready
      note "create user and database"
      mysql -uroot -e "CREATE USER IF NOT EXISTS 'pa'@'127.0.0.1' IDENTIFIED BY 'pa-test-only'; GRANT ALL ON *.* TO 'pa'@'127.0.0.1' WITH GRANT OPTION; CREATE DATABASE \`full\`;"
    fi
    note "ballast and filler"
    sync
    dd if=/dev/zero of="$datadir/$ballast_name" bs=1M count=8 status=none
    chown "$owner:$owner" "$datadir/$ballast_name"
    sync
    # Leave ROOM_MB for the test to fill (the ballast is part of what is left, so it is freed on top).
    avail=$(df -k --output=avail "$datadir" | tail -1 | tr -d ' ')
    fill=$((avail - room * 1024))
    if [ "$fill" -gt 0 ]; then
      dd if=/dev/zero of="$datadir/filler" bs=1k count="$fill" status=none
      chown "$owner:$owner" "$datadir/filler"
    fi
    sync
    df -k "$datadir" | tail -1
    ;;
  free)
    note "delete the ballast, then start the server if the full disk stopped it"
    rm -f "$datadir/$ballast_name"
    sync
    # PostgreSQL PANICs when a WAL write fails and then cannot finish crash recovery on a full volume,
    # so it stays down until space is freed AND someone starts it: what an operator does.
    start >&2 || true
    ;;
  off)
    note "stop, unmount, remove, restart the normal server"
    stop
    umount -l "$datadir" 2>/dev/null || true
    rm -f "$img" /etc/mysql/mysql.conf.d/zz-pa-small.cnf
    start || true
    ;;
  *) echo "unknown op $op" >&2; exit 2 ;;
esac
