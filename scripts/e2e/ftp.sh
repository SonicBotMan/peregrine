#!/usr/bin/env bash
# FTP e2e: full download (md5) + mid-flight kill -9 (REST resume, md5).
# Spins a local pyftpdlib server; needs `python3 -m pip install --user pyftpdlib`.
source "$(dirname "$0")/common.sh"

FTP_ROOT="$QA_DIR/ftp/root"; mkdir -p "$FTP_ROOT"
cat >"$QA_DIR/ftp/ftpd.py" <<EOF
from pyftpdlib.authorizers import DummyAuthorizer
from pyftpdlib.handlers import FTPHandler
from pyftpdlib.servers import FTPServer
a = DummyAuthorizer(); a.add_user("qa", "qapass", "$FTP_ROOT", perm="elr")
h = FTPHandler; h.authorizer = a; h.passive_ports = range(62000, 62100)
FTPServer(("127.0.0.1", 2121), h).serve_forever()
EOF

head -c 314572800 /dev/zero | openssl enc -aes-256-ctr \
    -K "$(printf '0%.0s' {1..64})" -iv "$(printf '0%.0s' {1..32})" \
    >"$FTP_ROOT/big.bin" 2>/dev/null
REF_MD5=$(md5_of "$FTP_ROOT/big.bin"); echo "ref md5: $REF_MD5"

( cd "$QA_DIR/ftp" && nohup python3 ftpd.py >ftpd.log 2>&1 & echo $! >ftpd.pid )
sleep 2
curl -s --max-time 8 "ftp://qa:qapass@127.0.0.1:2121/" >/dev/null || fail "ftp server not up"

start_daemon "$QA_DIR/ftp/db.sqlite"

# --- Part 1: full download ---
ID=$(pg add "ftp://qa:qapass@127.0.0.1:2121/big.bin" -o "$QA_DIR/ftp/dl-full.bin" | awk '{print $2}')
wait_status "$ID" completed 120 || fail "full download did not complete"
[[ $(md5_of "$QA_DIR/ftp/dl-full.bin") == "$REF_MD5" ]] && pass "ftp full: md5 match" \
    || fail "ftp full: md5 mismatch"

# --- Part 2: kill -9 mid-flight, restart, REST resume ---
ID=$(pg add "ftp://qa:qapass@127.0.0.1:2121/big.bin" -o "$QA_DIR/ftp/dl-resume.bin" | awk '{print $2}')
sleep 1
PRE=$(received "$ID"); echo "pre-kill received=$PRE"
(( PRE > 0 && PRE < 314572800 )) || fail "kill window missed (pre=$PRE)"
kill_daemon9
start_daemon "$QA_DIR/ftp/db.sqlite"
wait_status "$(tid_of dl-resume)" completed 300 || fail "resume did not complete"
[[ $(md5_of "$QA_DIR/ftp/dl-resume.bin") == "$REF_MD5" ]] && pass "ftp kill-9 resume: md5 match" \
    || fail "ftp kill-9 resume: md5 mismatch"

kill_daemon9; stop_bg "$QA_DIR/ftp/ftpd.pid"
echo "ALL FTP E2E PASS"
