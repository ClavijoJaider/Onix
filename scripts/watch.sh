#!/bin/bash
set -u
export PATH="$HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
. "$ROOT/scripts/fingerprint.sh"
STATE="$HOME/.cache/mi-terminal-autobuild"
mkdir -p "$STATE"
rd() { cat "$STATE/$1" 2>/dev/null; }

fp="$(fingerprint "$ROOT")"
[ "$fp" = "$(rd last-fingerprint)" ] && exit 0
[ "$fp" = "$(rd last-failed-fingerprint)" ] && exit 0

LOCK="$STATE/watch.lock"
if ! mkdir "$LOCK" 2>/dev/null; then
  kill -0 "$(cat "$LOCK/pid" 2>/dev/null)" 2>/dev/null && exit 0
  rm -rf "$LOCK"; mkdir "$LOCK" || exit 1
fi
echo $$ > "$LOCK/pid"
trap 'rm -rf "$LOCK"' EXIT

# debounce: 10 s de calma, máximo 2 min
start=$SECONDS
while [ $((SECONDS - start)) -lt 120 ]; do
  sleep 10
  new="$(fingerprint "$ROOT")"
  [ "$new" = "$fp" ] && break
  fp="$new"
done
[ "$fp" = "$(rd last-fingerprint)" ] && exit 0
[ "$fp" = "$(rd last-failed-fingerprint)" ] && exit 0
bash "$ROOT/scripts/build-app.sh"
