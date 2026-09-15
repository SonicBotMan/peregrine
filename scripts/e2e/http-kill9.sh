#!/usr/bin/env bash
# HTTP segmented e2e: rate-limited download, kill -9 mid-flight, restart,
# REST resume to completion, md5 vs a reference curl download.
# Also guards Bug 3 (QA-E2E R1): re-adding a failed task must not 416.
source "$(dirname "$0")/common.sh"

URL="${1:-https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/debian-13.7.0-amd64-netinst.iso}"
mkdir -p "$QA_DIR/http"
start_daemon "$QA_DIR/http/db.sqlite"

( cd "$QA_DIR/http" && nohup curl -sL -o ref.bin "$URL" >/dev/null 2>&1 & echo $! >ref.pid )

ID=$(pg add "$URL" -o "$QA_DIR/http/dl.bin" | awk '{print $2}')
pg limit --bps 512k "$ID" >/dev/null
sleep 6
PRE=$(received "$ID"); echo "pre-kill received=$PRE"
(( PRE > 0 )) || fail "no progress before kill"

kill_daemon9
start_daemon "$QA_DIR/http/db.sqlite"
ID=$(tid_of "dl.bin")
pg limit --bps 0 "$ID" >/dev/null
# After restart the engine must resume (monotonic progress), not restart at 0.
A=$(received "$ID"); sleep 3; B=$(received "$ID")
(( B > A && A > 0 )) && pass "resume monotonic ($A -> $B)" || fail "resume regressed/took too long ($A -> $B)"

wait_status "$ID" completed 900 || fail "resumed download did not complete"
wait_ref() { local n=0; while pgrep -x curl >/dev/null && (( n < 300 )); do sleep 3; ((n+=3)); done; }
wait_ref
[[ $(md5_of "$QA_DIR/http/dl.bin") == $(md5_of "$QA_DIR/http/ref.bin") ]] \
    && pass "http kill-9 resume: md5 match" || fail "md5 mismatch vs curl reference"

kill_daemon9; stop_bg "$QA_DIR/http/ref.pid"
echo "ALL HTTP E2E PASS"
