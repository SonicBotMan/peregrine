#!/usr/bin/env bash
# Shared helpers for Peregrine e2e scripts (real-network, manual runs; NOT in CI).
# Lessons baked in (see docs/reviews/QA-E2E-R1.md "流程教训"):
#   - task ids are <random-prefix>-<seq>: always resolve dynamically, never hardcode.
#   - pkill -f suicides if the same command line matches: use pkill_safe.
#   - `cmd &` swallows a preceding cd: use bg() which wraps a subshell with explicit cd.
set -uo pipefail

PG_BIN="${PG_BIN:-$HOME/projects/peregrine/target/release/pg}"
DAEMON_BIN="${DAEMON_BIN:-$HOME/projects/peregrine/target/release/peregrined}"
QA_DIR="${QA_DIR:-/tmp/pg-e2e}"
SOCKET="${SOCKET:-tcp:8500}"

pass() { echo "PASS: $*"; }
fail() {
    echo "FAIL: $*"
    exit 1
}

pg() { "$PG_BIN" --socket "$SOCKET" "$@"; }

# Resolve the task id of the most recent task whose URL/out contains $1.
tid_of() { # tid_of <needle>
    local id
    id=$(pg list | grep "$1" | awk '{print $1}' | tail -1)
    [[ -n "$id" ]] || fail "no task matches '$1'"
    echo "$id"
}

wait_status() { # wait_status <id> <expected> [timeout_s=600] [poll_s=3]
    local id=$1 want=$2 tmo=${3:-600} poll=${4:-3} st elapsed=0
    while :; do
        st=$(pg get "$id" | grep -oP '(?<="status": ")[a-z]+' | head -1)
        [[ $st == "$want" ]] && return 0
        [[ $st == failed ]] && {
            pg get "$id" | grep -E '"error"'
            return 1
        }
        ((elapsed += poll))
        ((elapsed >= tmo)) && {
            echo "timeout waiting $want (last=$st)"
            return 1
        }
        sleep "$poll"
    done
}

received() { pg get "$1" | grep -oP '(?<="received_bytes": )[0-9]+' | head -1; }

kill_daemon9() {
    pkill -9 -x peregrined 2>/dev/null
    sleep 1
}

start_daemon() { # start_daemon <db>
    (cd "$QA_DIR" && nohup "$DAEMON_BIN" --listen "$SOCKET" --db "$1" >daemon.log 2>&1 &)
    sleep 2
    pg ping >/dev/null 2>&1 || fail "daemon did not start"
}

# Stop a background server tracked in a pidfile (avoids pkill -f self-match suicide).
stop_bg() { # stop_bg <pidfile>
    local f=$1
    [[ -f $f ]] && kill "$(cat "$f")" 2>/dev/null
    rm -f "$f"
}

bg_pid() { echo $!; }

md5_of() { md5sum "$1" | awk '{print $1}'; }
