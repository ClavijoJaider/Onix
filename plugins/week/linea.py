#!/usr/bin/env python3
import json, math, os, sys
from datetime import datetime

DIAS = "lun mar mié jue vie sáb dom".split()

def when(r, day):
    # Tiempo que falta para el reinicio: "4h 50m", "2d 15h", "12m". resets_at: epoch s o ISO.
    try:
        t = float(r)
    except Exception:
        t = datetime.fromisoformat(str(r).replace("Z", "+00:00")).timestamp()
    m = max(0, int((t - datetime.now().timestamp()) // 60))
    d, h, mi = m // 1440, m % 1440 // 60, m % 60
    return f"↻ {d}d {h}h" if d else (f"↻ {h}h {mi}m" if h else f"↻ {mi}m")

def seg(name, w, day, c=208):
    try:
        # Claude Code manda el % con un decimal (Math.round(u*1000)/10); claude.ai lo muestra
        # redondeado hacia arriba, así que se usa ceil para que coincidan (5.2 -> 6 %).
        p = max(0, min(100, math.ceil(float(w["used_percentage"]) - 1e-9)))
        n = min(10, math.ceil(p / 10)) if p > 0 else 0  # 10 tramos; desde 1 % ya se ve uno
        return f"\033[38;5;255m{name}\033[0m \033[38;5;{c}m{'━'*n}\033[38;5;240m{'─'*(10-n)}\033[0m \033[38;5;{c}m{p}%\033[0m \033[38;5;248m{when(w['resets_at'], day)}\033[0m"
    except Exception:
        return f"\033[38;5;255m{name}\033[0m —"

def titulo(d):
    # Nombre de sesión: session_name, o último custom-title/ai-title del transcript; si no, carpeta.
    n = d.get("session_name")
    try:
        for l in open(d["transcript_path"], errors="ignore"):
            if '-title"' in l:
                e = json.loads(l)
                n = n if e.get("type") == "ai-title" and n else e.get("customTitle") or e.get("aiTitle") or n
    except Exception:
        pass
    return n or os.path.basename((d.get("workspace") or {}).get("current_dir") or d.get("cwd") or "") or "claude"

try:
    d = json.load(sys.stdin)
    rl = d.get("rate_limits") or {}
except Exception:
    d, rl = {}, {}
try:
    with open("/dev/tty", "w") as t:  # título de la pestaña (OSC 0 y 2)
        t.write(f"\033]0;{titulo(d)}\007\033]2;{titulo(d)}\007")
except Exception:
    pass
if not isinstance(rl, dict): rl = {}
print("   ".join([seg("5h", rl.get("five_hour"), False, 208), seg("week", rl.get("seven_day"), True)]))
