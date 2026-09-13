#!/usr/bin/env bash
# M3-b1 smoke: global + per-task rate limits, observed end-to-end.
# TUNA is a LAN-speed mirror (~0.5 GB/s), so every phase SETS the
# limit BEFORE the download can finish; the 1.6G iso gives ~3.7h of
# runway at 128 KB/s.
set -u
cd "$(dirname "$0")/.."
export PGRG_SOCKET=/tmp/pg-smoke-b1.sock
export PGRG_DB=/tmp/pg-smoke-b1.db
rm -f "$PGRG_SOCKET" "$PGRG_DB" "$PGRG_DB"-* /tmp/smoke-b1.*
PG=~/projects/peregrine/target/debug

"$PG/peregrined" --socket "$PGRG_SOCKET" --db "$PGRG_DB" & DAEMON=$!
trap 'kill $DAEMON 2>/dev/null; wait $DAEMON 2>/dev/null' EXIT
for _ in $(seq 60); do [ -S "$PGRG_SOCKET" ] && break; sleep 0.2; done

URL=https://mirrors.tuna.tsinghua.edu.cn/archlinux/iso/latest/archlinux-x86_64.iso
read_bytes() {
    curl -s --unix-socket "$PGRG_SOCKET" "http://localhost/tasks/$1" |
        grep -oE '"received_bytes":[0-9]+' | grep -oE '[0-9]+'
}

echo "== phase 1: global 512k from the first byte =="
"$PG/pg" --socket "$PGRG_SOCKET" speed --set 512k
ID=$("$PG/pg" --socket "$PGRG_SOCKET" add "$URL" --out /tmp/smoke-b1.iso | awk '{print $2}')
R0=$(read_bytes "$ID"); sleep 8; R1=$(read_bytes "$ID")
echo "  +$(( (R1 - R0) / 8 / 1024 )) KB/s (expect ~512)"

echo "== phase 2: live-tighten global to 128k =="
"$PG/pg" --socket "$PGRG_SOCKET" speed --set 128k
R2=$(read_bytes "$ID"); sleep 8; R3=$(read_bytes "$ID")
echo "  +$(( (R3 - R2) / 8 / 1024 )) KB/s (expect ~128)"

echo "== phase 3: unlimited global, per-task 128k (paused hand-off: no gap at full speed) =="
"$PG/pg" --socket "$PGRG_SOCKET" pause "$ID"
sleep 0.5
"$PG/pg" --socket "$PGRG_SOCKET" speed --set 0
"$PG/pg" --socket "$PGRG_SOCKET" limit "$ID" --bps 128k
"$PG/pg" --socket "$PGRG_SOCKET" resume "$ID"
R4=$(read_bytes "$ID"); sleep 8; R5=$(read_bytes "$ID")
echo "  +$(( (R5 - R4) / 8 / 1024 )) KB/s (expect ~128 + burst)"

echo "== phase 4: clear per-task limit -> full LAN speed =="
"$PG/pg" --socket "$PGRG_SOCKET" limit "$ID" --bps 0
R6=$(read_bytes "$ID"); sleep 4; R7=$(read_bytes "$ID")
echo "  +$(( (R7 - R6) / 4 / 1024 )) KB/s (expect >20000)"

echo "== persistence: settings survive a restart =="
"$PG/pg" --socket "$PGRG_SOCKET" speed --set 256k
STATUS=$("$PG/pg" --socket "$PGRG_SOCKET" get "$ID" | awk '{print $2}')
kill $DAEMON; wait $DAEMON 2>/dev/null
for _ in $(seq 60); do [ -S "$PGRG_SOCKET" ] && break; sleep 0.2; done
"$PG/peregrined" --socket "$PGRG_SOCKET" --db "$PGRG_DB" & DAEMON=$!
for _ in $(seq 60); do [ -S "$PGRG_SOCKET" ] && break; sleep 0.2; done
echo "  after restart: speed -> $("$PG/pg" --socket "$PGRG_SOCKET" speed)"
echo "  task after restart: $STATUS -> $("$PG/pg" --socket "$PGRG_SOCKET" get "$ID" | awk '{print $2}')"

"$PG/pg" --socket "$PGRG_SOCKET" remove "$ID"
echo SMOKE_OK
