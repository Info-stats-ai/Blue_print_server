#!/usr/bin/env bash
# Adds one more person to the demo that start-demo.sh is running, in a real
# I app, so someone can watch or take part live (for example as the manager).
#
#   ./demo/join-demo.sh you [path/to/I.app] [folder-name]
#
# folder-name puts their copy of the project in a differently named folder,
# as happens on a real teammate's computer.
#
# The person joins the same session and gets their own copy of the project.
# Defaults to ~/i-ide/target/tester/dmg/I.app (made by script/package-tester-mac).
set -euo pipefail

DEMO="$(cd "$(dirname "$0")" && pwd)"
STATE="$DEMO/.state"
NAME="${1:?usage: join-demo.sh <name> [path/to/I.app]}"
APP="${2:-$HOME/i-ide/target/tester/dmg/I.app}"
FOLDER="${3:-}"
PORT=3917
BASE="http://127.0.0.1:$PORT"
[ -f "$STATE/session_id" ] || { echo "No demo running: start one with start-demo.sh"; exit 1; }
[ -d "$APP" ] || { echo "No app at $APP"; exit 1; }
SESSION=$(cat "$STATE/session_id")
INVITE=$(sqlite3 "$STATE/demo.db" "select invite_token from sessions where id = '$SESSION'")
LEAD_EMAIL=$(sqlite3 "$STATE/demo.db" "select u.email from sessions s join users u on u.id = s.owner_id where s.id = '$SESSION'")
json() { python3 -I -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
post() { curl -s -X "${3:-POST}" "$BASE$1" -H 'content-type: application/json' ${TOKEN:+-H "authorization: Bearer $TOKEN"} -d "$2"; }

reply=$(TOKEN= post /auth/register "{\"email\":\"$NAME@example.test\",\"password\":\"demo-only-password\"}")
TOKEN_NEW=$(echo "$reply" | json 'd.get("token","")')
if [ -z "$TOKEN_NEW" ]; then
  reply=$(TOKEN= post /auth/login "{\"email\":\"$NAME@example.test\",\"password\":\"demo-only-password\"}")
  TOKEN_NEW=$(echo "$reply" | json 'd["token"]')
fi
USER_ID=$(echo "$reply" | json 'd["user_id"]')
LEAD_TOKEN=$(TOKEN= post /auth/login "{\"email\":\"$LEAD_EMAIL\",\"password\":\"demo-only-password\"}" | json 'd["token"]')
TOKEN=$TOKEN_NEW post "/sessions/$SESSION/join" "{\"invite\":\"$INVITE\"}" >/dev/null
TOKEN=$LEAD_TOKEN post "/sessions/$SESSION/join-requests/$USER_ID" '{"approve":true}' PUT >/dev/null

HOME_DIR="$STATE/$NAME"
mkdir -p "$HOME_DIR/data/config"
# A copy of the lead's project as it is now, so the newcomer starts level.
LEAD="${LEAD_EMAIL%@*}"
LEAD_PROJECT=$(find "$STATE/$LEAD" -mindepth 1 -maxdepth 1 -type d ! -name data | head -1)
PROJECT_DIR="$HOME_DIR/${FOLDER:-$(basename "$LEAD_PROJECT")}"
if [ ! -d "$PROJECT_DIR" ]; then
  rsync -a --exclude .i-ide --exclude __pycache__ "$LEAD_PROJECT/" "$PROJECT_DIR/"
fi
cat >"$HOME_DIR/data/config/settings.json" <<'JSON'
{
  "collab_mode": "multi",
  "session": { "trust_all_worktrees": true },
  "agent": { "default_model": { "provider": "anthropic", "model": "claude-sonnet-5-5" } }
}
JSON
# Same program as the others, its own hidden app ID (see start-demo.sh).
COPY="$HOME/i-ide/target/demo/I.$NAME.app"
rm -rf "$COPY"; mkdir -p "$COPY/Contents/MacOS"
cp -R "$APP/Contents/Resources" "$COPY/Contents/"
cp "$APP/Contents/Info.plist" "$COPY/Contents/Info.plist"
ln "$APP/Contents/MacOS/i-ide" "$COPY/Contents/MacOS/i-ide" 2>/dev/null || cp "$APP/Contents/MacOS/i-ide" "$COPY/Contents/MacOS/i-ide"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier dev.iide.I.$NAME" "$COPY/Contents/Info.plist"
codesign --force --sign - "$COPY" >/dev/null 2>&1 || true
open -n "$COPY" \
  --env BLUEPRINT_HTTP_URL="$BASE" --env BLUEPRINT_SERVER_URL="ws://127.0.0.1:$PORT/ws" \
  --env BLUEPRINT_DEV_TOKEN="$TOKEN_NEW" --env BLUEPRINT_DEV_USER_ID="$USER_ID" \
  --env I_IDE_API_URL="$BASE/api" --env ZED_STATELESS=1 \
  --stdout "$HOME_DIR/ide.log" --stderr "$HOME_DIR/ide.log" \
  --args --user-data-dir "$HOME_DIR/data" "$PROJECT_DIR" "i-ide://i-mode/join/$SESSION.$INVITE"
echo "$NAME joined the demo in $COPY, with a copy of the project at $PROJECT_DIR"
