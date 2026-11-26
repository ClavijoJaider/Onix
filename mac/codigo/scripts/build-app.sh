#!/bin/bash
set -u
export PATH="$HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
. "$ROOT/scripts/fingerprint.sh"
STATE="$HOME/.cache/mi-terminal-autobuild"
LOG="$HOME/Library/Logs/mi-terminal-build.log"
APPS="$HOME/Applications"
APP="$APPS/Onix.app"
mkdir -p "$STATE" "$APPS" "$(dirname "$LOG")"

notify() { osascript -e "display notification \"$1\" with title \"Ónix\"" >/dev/null 2>&1; }

# lock anti-simultáneo (mkdir es atómico); recupera locks de procesos muertos
LOCK="$STATE/build.lock"
if ! mkdir "$LOCK" 2>/dev/null; then
  if kill -0 "$(cat "$LOCK/pid" 2>/dev/null)" 2>/dev/null; then echo "build ya en curso"; exit 0; fi
  rm -rf "$LOCK"; mkdir "$LOCK" || exit 1
fi
echo $$ > "$LOCK/pid"
TMP=""
cleanup() { rm -rf "$LOCK" ${TMP:+"$TMP"}; if [ -f "$LOG" ]; then tail -n 500 "$LOG" > "$LOG.tmp" && mv "$LOG.tmp" "$LOG"; fi; }
trap cleanup EXIT
exec >>"$LOG" 2>&1

echo "=== $(date '+%F %T') build ==="
FP="$(fingerprint "$ROOT")"   # antes del build: si algo cambia durante él, se reconstruye luego
cd "$ROOT" || exit 1
if ! nice cargo build --release; then
  echo "BUILD FALLIDO (app intacta)"
  echo "$FP" > "$STATE/last-failed-fingerprint"
  notify "Falló el build de mi-terminal (ver mi-terminal-build.log)"
  exit 1
fi

TMP="$(mktemp -d "$APPS/.mt-build.XXXXXX")" || exit 1
N="$TMP/Onix.app"
mkdir -p "$N/Contents/MacOS" "$N/Contents/Resources"
ICON="$ROOT/crates/app/assets/AppIcon.icns"
[ -f "$ICON" ] || python3 "$ROOT/scripts/make-icon.py" "$ICON" || exit 1
cp "$ICON" "$N/Contents/Resources/AppIcon.icns" || exit 1
cp "${CARGO_TARGET_DIR:-$ROOT/target}/release/onix" "$N/Contents/MacOS/onix" || exit 1
cat > "$N/Contents/Info.plist" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.mi-terminal.app</string>
<key>CFBundleExecutable</key><string>onix</string>
<key>CFBundleIconFile</key><string>AppIcon</string>
<key>CFBundleName</key><string>Ónix</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1</string>
<key>NSHighResolutionCapable</key><true/>
<key>LSMinimumSystemVersion</key><string>11.0</string>
</dict></plist>
PL
if ! codesign --force -s - "$N"; then
  echo "CODESIGN FALLIDO (app intacta)"; notify "Falló codesign de mi-terminal"; exit 1
fi

rm -rf "$APP.old"
[ -e "$APP" ] && { mv "$APP" "$APP.old" || exit 1; }
if ! mv "$N" "$APP"; then
  [ -e "$APP.old" ] && mv "$APP.old" "$APP"
  echo "REEMPLAZO FALLIDO"; notify "No se pudo reemplazar Onix.app"; exit 1
fi
rm -rf "$APP.old"
echo "$FP" > "$STATE/last-fingerprint"
rm -f "$STATE/last-failed-fingerprint"
echo "OK app actualizada"
if pgrep -x onix >/dev/null; then notify "Hay versión nueva de Ónix, reábrela"; fi
exit 0
