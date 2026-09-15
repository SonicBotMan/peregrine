#!/usr/bin/env bash
# BT e2e: Debian official netinst torrent, full download, sha256 vs official.
# Public network (DHT/trackers) required; takes seconds with good peers.
source "$(dirname "$0")/common.sh"

TORRENT_URL="https://cdimage.debian.org/debian-cd/current/amd64/bt-cd/debian-13.7.0-amd64-netinst.iso.torrent"
SHA_URL="https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/SHA256SUMS"
mkdir -p "$QA_DIR/bt"; cd "$QA_DIR/bt" || exit 1

curl -sO "$TORRENT_URL" || fail "cannot fetch torrent"
curl -sL "$SHA_URL" | grep 'netinst.iso$' >official.sha256 || fail "cannot fetch sha256"
WANT=$(awk '{print $1}' official.sha256)

start_daemon "$QA_DIR/bt/db.sqlite"
ID=$(pg add "$QA_DIR/bt/debian-13.7.0-amd64-netinst.iso.torrent" -o "$QA_DIR/bt/out/" | awk '{print $2}')
wait_status "$ID" completed 1800 || fail "torrent did not complete"

GOT=$(sha256sum "$QA_DIR/bt/out/debian-13.7.0-amd64-netinst.iso" | awk '{print $1}')
[[ $GOT == "$WANT" ]] && pass "bt: sha256 matches official" || fail "sha256 mismatch: $GOT vs $WANT"

kill_daemon9
echo "ALL BT E2E PASS"
