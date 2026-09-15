#!/usr/bin/env bash
# HLS e2e: VOD full download (TS structure + md5 across two runs) and live record.
# Public test streams; depends on network reachability.
source "$(dirname "$0")/common.sh"

VOD_URL="https://devstreaming-cdn.apple.com/videos/streaming/examples/img_bipbop_adv_example_ts/master.m3u8"
LIVE_URL="https://demo.unified-streaming.com/k8s/live/stable/scte35.isml/.m3u8"
mkdir -p "$QA_DIR/hls"
start_daemon "$QA_DIR/hls/db.sqlite"

# --- Part 1: VOD twice, byte-identical ---
ID=$(pg add "$VOD_URL" -o "$QA_DIR/hls/vod1.ts" | awk '{print $2}')
wait_status "$ID" completed 900 || fail "vod run1 failed"
ID=$(pg add "$VOD_URL" -o "$QA_DIR/hls/vod2.ts" | awk '{print $2}')
wait_status "$ID" completed 900 || fail "vod run2 failed"
M1=$(md5_of "$QA_DIR/hls/vod1.ts")
M2=$(md5_of "$QA_DIR/hls/vod2.ts")
[[ $M1 == "$M2" ]] && pass "vod: two runs identical ($M1)" || fail "vod: md5 mismatch $M1 vs $M2"
SZ=$(stat -c %s "$QA_DIR/hls/vod1.ts")
python3 - "$QA_DIR/hls/vod1.ts" <<'EOF' || fail "vod: TS structure check failed"
import random, sys
sz = os.path.getsize(p := sys.argv[1]) if (os := __import__('os')) else 0
assert sz % 188 == 0, f"size {sz} not multiple of 188"
with open(p, 'rb') as f:
    for i in sorted(random.Random(1).sample(range(sz // 188), 500)):
        f.seek(i * 188)
        assert f.read(1) == b'\x47', f"bad sync at packet {i}"
EOF
pass "vod: TS structure ok ($SZ bytes)"

# --- Part 2: live record ~45s then cancel; assert a finalized output exists.
# QA-E2E Bug 1 FIXED (salvage_merge): cancel must finalize a playable
# file from the recorded parts. This block is the regression guard.
ID=$(pg add "$LIVE_URL" -o "$QA_DIR/hls/live.ts" | awk '{print $2}')
sleep 45
R=$(received "$ID")
((R > 5_000_000)) || fail "live: recorded too little ($R)"
pg remove "$ID" >/dev/null
sleep 5
if [[ -f "$QA_DIR/hls/live.ts" ]]; then
    pass "live: finalize on cancel ($(stat -c %s "$QA_DIR/hls/live.ts") bytes)"
else
    fail "live: REGRESSION — cancel did not finalize output (parts left on disk)"
fi

kill_daemon9
echo "ALL HLS E2E DONE"
