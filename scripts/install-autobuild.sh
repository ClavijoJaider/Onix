#!/bin/bash
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LABEL=com.mi-terminal.autobuild
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
cat > "$PLIST" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>$LABEL</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>$ROOT/scripts/watch.sh</string></array>
<key>StartInterval</key><integer>30</integer>
<key>ProcessType</key><string>Background</string>
<key>Nice</key><integer>10</integer>
<key>LowPriorityIO</key><true/>
<key>EnvironmentVariables</key><dict><key>PATH</key><string>$HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin</string></dict>
<key>RunAtLoad</key><true/>
<key>WorkingDirectory</key><string>$ROOT</string>
<key>StandardOutPath</key><string>$HOME/Library/Logs/mi-terminal-autobuild.out</string>
<key>StandardErrorPath</key><string>$HOME/Library/Logs/mi-terminal-autobuild.out</string>
</dict></plist>
PL
plutil -lint "$PLIST" || exit 1
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null
bash "$ROOT/scripts/build-app.sh" || echo "primer build falló (ver ~/Library/Logs/mi-terminal-build.log); el agente queda instalado"
launchctl bootstrap "gui/$(id -u)" "$PLIST" || exit 1
echo "instalado: $LABEL"
