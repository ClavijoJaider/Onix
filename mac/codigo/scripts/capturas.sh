#!/bin/bash
# Abre una ventana limpia de Ónix y arma sola cada vista para tomar las capturas a mano.
# Correr desde la Terminal de macOS, con permiso de Accesibilidad.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEMO="$HOME/onix-demo"
mkdir -p "$DEMO"
sips -s format png "$ROOT/crates/app/assets/AppIcon.icns" --out "$DEMO/icono.png" >/dev/null

cd "$DEMO"
MI_TERMINAL_CMD='cd ~/onix-demo && clear && ls "$PWD"/*.png' nohup "$HOME/Applications/Onix.app/Contents/MacOS/onix" >/dev/null 2>&1 &
PID=$!
sleep 3

# Trae al frente la ventana de muestra y manda las teclas solo si de verdad quedó al frente.
enviar() {
  osascript >/dev/null <<APPLESCRIPT || { echo "No pude pulsar teclas. Activa Terminal en Accesibilidad, ciérrala con ⌘Q y repite."; exit 1; }
tell application "System Events"
  set frontmost of (first process whose unix id is $PID) to true
  delay 0.5
  if unix id of (first process whose frontmost is true) is not $PID then error "la ventana de muestra no está al frente"
  $1
end tell
APPLESCRIPT
  sleep 1.5
}
atajo() { enviar "keystroke \"$1\" using {$2}"; }
escribir() { enviar "keystroke \"$1\"
  key code 36"; }
pausa() {
  printf '\n== %s ==\n%s\n' "$1" "$2"
  read -r -p "Toma la captura (⌃⇧⌘4), vuelve aquí y pulsa Enter... " _
}

# Segunda pestaña con un listado, y la primera dividida con Claude a la derecha.
atajo t "command down"
escribir "cd ~/onix-demo && clear && ls -l"
atajo 1 "command down"
atajo d "command down"
escribir "cd ~/onix-demo && clear && claude"
pausa "1. Principal" "Agranda la ventana. Si Claude pregunta si confías en la carpeta, acepta."

atajo "[" "command down"
pausa "2. Imagen" "Deja el ratón sobre la ruta de icono.png hasta ver la vista previa."

atajo u "command down, shift down"
pausa "3. Consumo" "Está abierto Agent usage. Muestra tus cifras reales."

enviar "key code 53"
atajo a "command down, shift down"
atajo p "command down, shift down"
pausa "4. Agentes y procesos" "Revisa que no se vean nombres privados en la lista de procesos."

echo "Listo. Cierra la ventana de muestra y pega las capturas en el chat."
