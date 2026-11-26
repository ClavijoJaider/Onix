# Plugins de la línea de estado
Cada carpeta con un `linea.py` es un plugin; `statusline.py` los une en orden alfabético.
- `contexto`: % de contexto consumido.
- `week`: uso de 5 h y semanal, con el tiempo para reiniciarse (también pone el título de la pestaña).

Activar, en `~/.claude/settings.json`:

    "statusLine": { "type": "command", "command": "python3 ~/Desktop/onix/mac/codigo/plugins/statusline.py" }

Volver a lo de antes: `python3 ~/.claude/statusline.py`.
