#!/bin/bash
set -u
LABEL=com.mi-terminal.autobuild
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null
rm -f "$HOME/Library/LaunchAgents/$LABEL.plist"
echo "desinstalado (la app no se toca)"
