#!/usr/bin/env bash
# Starts a fresh I-mode demo: a local blueprint-server, test accounts in one
# session (the first person leads, the rest join and are approved), and an IDE
# window for each.
#
#   DEMO_PEOPLE="alice bob carol dave" DEMO_PROJECT=project-bookshelf ./start-demo.sh
#
# Defaults: two people (alice, bob) and the tiny project-template.
set -euo pipefail

DEMO="$(cd "$(dirname "$0")" && pwd)"
STATE="$DEMO/.state"
IDE="$HOME/i-ide/target/debug/i-ide"
SERVER_DIR="$HOME/blueprint-server"
SERVER="$SERVER_DIR/target/debug/blueprint-server"
CREDENTIALS="$HOME/.config/i-ide/development_credentials"
PORT=3917
PEOPLE=(${DEMO_PEOPLE:-alice bob})
PROJECT="${DEMO_PROJECT:-project-template}"
[ -d "$DEMO/$PROJECT" ] || { echo "No project folder: $DEMO/$PROJECT"; exit 1; }
BASE="http://127.0.0.1:$PORT"

[ -x "$IDE" ] || { echo "IDE not built: $IDE"; exit 1; }
[ -x "$SERVER" ] || { echo "Server not built: $SERVER"; exit 1; }

"$DEMO/stop-demo.sh" >/dev/null 2>&1 || true
rm -rf "$STATE"
mkdir -p "$STATE"

# Run from the server repo so it picks up ANTHROPIC_API_KEY from its .env
# (for @agent); everything else is local to this demo.
# @agent runs on the server; give it the Anthropic key your IDE already uses
# (process env only, never written to disk) so it works even if .env is stale.
IDE_ANTHROPIC_KEY=""
if [ -f "$CREDENTIALS" ]; then
  IDE_ANTHROPIC_KEY=$(python3 -I -c '
import json, sys
creds = json.load(open(sys.argv[1]))
entry = creds.get("https://api.anthropic.com")
print(bytes(entry[1]).decode() if entry else "")
' "$CREDENTIALS")
fi

cd "$SERVER_DIR"
if [ -n "$IDE_ANTHROPIC_KEY" ]; then export ANTHROPIC_API_KEY="$IDE_ANTHROPIC_KEY"; fi
DATABASE_URL="sqlite:$STATE/demo.db" JWT_SECRET=demo-local-only PORT=$PORT \
  nohup "$SERVER" >"$STATE/server.log" 2>&1 </dev/null &
unset ANTHROPIC_API_KEY IDE_ANTHROPIC_KEY
echo $! >"$STATE/server.pid"
cd "$DEMO"
for _ in $(seq 1 60); do
  curl -s -o /dev/null "$BASE/sessions" && break
  sleep 0.5
done
curl -s -o /dev/null "$BASE/sessions" || { echo "Server didn't start; see $STATE/server.log"; exit 1; }

json() { python3 -I -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
register() {
  curl -s -X POST "$BASE/auth/register" -H 'content-type: application/json' \
    -d "{\"email\":\"$1\",\"password\":\"demo-only-password\"}"
}

declare -a TOKENS IDS
for index in "${!PEOPLE[@]}"; do
  reply=$(register "${PEOPLE[$index]}@example.test")
  TOKENS[$index]=$(echo "$reply" | json 'd["token"]')
  IDS[$index]=$(echo "$reply" | json 'd["user_id"]')
done
LEAD_TOKEN=${TOKENS[0]}

CREATED=$(curl -s -X POST "$BASE/sessions" -H "authorization: Bearer $LEAD_TOKEN" \
  -H 'content-type: application/json' \
  -d "{\"name\":\"$(basename "$PROJECT" | sed "s/^project-//")\",\"goal\":\"Build it together\",\"has_leader\":true}")
SESSION=$(echo "$CREATED" | json 'd["session"]["id"]')
INVITE=$(echo "$CREATED" | json 'd["invite_token"]')
for index in "${!PEOPLE[@]}"; do
  [ "$index" = 0 ] && continue
  curl -s -o /dev/null -X POST "$BASE/sessions/$SESSION/join" -H "authorization: Bearer ${TOKENS[$index]}" \
    -H 'content-type: application/json' -d "{\"invite\":\"$INVITE\"}"
  curl -s -o /dev/null -X PUT "$BASE/sessions/$SESSION/join-requests/${IDS[$index]}" -H "authorization: Bearer $LEAD_TOKEN" \
    -H 'content-type: application/json' -d '{"approve":true}'
done
echo "$SESSION" >"$STATE/session_id"

# Each person's window is its own .app wrapping the dev binary (a hard link, no
# copy), so macOS treats them as separate apps: their own Dock icon ("I – Bob"),
# screen-recording picker entry, and automation can tell them apart. The
# wrappers must live inside the i-ide checkout: dev builds find their assets via
# its .git.
make_app() {
  local name=$1 label=$2
  local app="$HOME/i-ide/target/demo/I-$label.app"
  rm -rf "$app"
  mkdir -p "$app/Contents/MacOS"
  ln "$IDE" "$app/Contents/MacOS/i-ide" 2>/dev/null || cp "$IDE" "$app/Contents/MacOS/i-ide"
  cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>dev.iide.demo.$name</string>
  <key>CFBundleName</key><string>I – $label</string>
  <key>CFBundleExecutable</key><string>i-ide</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
  echo "$app"
}
rm -rf "$HOME/i-ide/target/demo/IIdeDev.app"

launch() {
  local name=$1 token=$2 user_id=$3
  local home="$STATE/$name"
  local label
  label="$(printf '%s' "${name:0:1}" | tr '[:lower:]' '[:upper:]')${name:1}"
  local APP
  APP=$(make_app "$name" "$label")
  mkdir -p "$home/data/config"
  local project_dir="$home/$(basename "$PROJECT" | sed 's/^project-//')"
  cp -R "$DEMO/$PROJECT" "$project_dir"
  # A git repo, like a real project, so the IDE shows changes.
  (cd "$project_dir" && git init -q && git add -A && git -c user.name=demo -c user.email=demo@example.test commit -qm "Start" ) >/dev/null 2>&1 || true
  cat >"$home/data/config/settings.json" <<'JSON'
{
  "collab_mode": "multi",
  "session": { "trust_all_worktrees": true },
  "extend_comment_on_newline": false
}
JSON
  # Linked, not copied, so the demo windows see your saved AI keys.
  [ -f "$CREDENTIALS" ] && ln -s "$CREDENTIALS" "$home/data/config/development_credentials"
  open -n "$APP" \
    --env BLUEPRINT_HTTP_URL="$BASE" --env BLUEPRINT_SERVER_URL="ws://127.0.0.1:$PORT/ws" \
    --env BLUEPRINT_DEV_TOKEN="$token" --env BLUEPRINT_DEV_USER_ID="$user_id" \
    --stdout "$home/ide.log" --stderr "$home/ide.log" \
    --args --user-data-dir "$home/data" "$project_dir" "i-ide://i-mode/join/$SESSION.$INVITE"
  # `open` returns before the app's pid is known; find it by its data dir.
  for _ in $(seq 1 40); do
    pid=$(pgrep -f -- "--user-data-dir $home/data" | head -1 || true)
    [ -n "$pid" ] && break
    sleep 0.25
  done
  echo "${pid:-}" >"$STATE/$name.pid"
}

for index in "${!PEOPLE[@]}"; do
  launch "${PEOPLE[$index]}" "${TOKENS[$index]}" "${IDS[$index]}"
  sleep 4
done

echo "Demo running: ${PEOPLE[*]} in one session (${PEOPLE[0]} leads), project $PROJECT."
echo "${#PEOPLE[@]} IDE windows are opening."
echo "Stop everything with: $DEMO/stop-demo.sh"
