#!/usr/bin/env bash
# record.sh <alice_window_id> <bob_window_id> <out_dir>
# Captures both windows about 4 times a second until <out_dir>/STOP exists.
# Frames are named by milliseconds since start so the real timing is kept.
A=$1; B=$2; OUT=$3
mkdir -p "$OUT/a" "$OUT/b"
rm -f "$OUT/STOP"
start=$(python3 -c 'import time; print(int(time.time()*1000))')
while [ ! -f "$OUT/STOP" ]; do
  now=$(python3 -c 'import time; print(int(time.time()*1000))')
  t=$(printf '%08d' $((now - start)))
  screencapture -o -x -t jpg -l "$A" "$OUT/a/$t.jpg" &
  screencapture -o -x -t jpg -l "$B" "$OUT/b/$t.jpg"
  wait
done
echo "stopped after $(( $(python3 -c 'import time; print(int(time.time()*1000))') - start )) ms"
