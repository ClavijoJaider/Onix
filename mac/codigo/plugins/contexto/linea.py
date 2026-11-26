#!/usr/bin/env python3
"""Línea de estado: % de la ventana de contexto consumido en la sesión."""
import json, math, sys

try:
    p = max(0, min(100, math.ceil(float(json.load(sys.stdin)["context_window"]["used_percentage"]) - 1e-9)))
    n = math.ceil(p / 10)
    c = 196 if p >= 80 else 208  # rojo cuando queda poco
    print(f"\033[38;5;255mctx\033[0m \033[38;5;{c}m{'━'*n}\033[38;5;240m{'─'*(10-n)}\033[0m \033[38;5;{c}m{p}%\033[0m")
except Exception:
    # Sin dato al empezar la sesión.
    print("\033[38;5;255mctx\033[0m —")
