#!/usr/bin/env bash
# Stops the demo's IDE windows and local server. Leaves your own IDE alone.
STATE="$(cd "$(dirname "$0")" && pwd)/.state"
for pid_file in "$STATE"/*.pid; do
  [ -f "$pid_file" ] && kill "$(cat "$pid_file")" 2>/dev/null
  rm -f "$pid_file"
done
echo "Demo stopped."
