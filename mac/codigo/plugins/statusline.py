#!/usr/bin/env python3
"""Une en una línea la salida de cada plugins/<nombre>/linea.py; todos reciben el mismo JSON de Claude Code."""
import pathlib, subprocess, sys

data = sys.stdin.buffer.read()
out = []
for f in sorted(pathlib.Path(__file__).parent.glob("*/linea.py")):
    try:
        r = subprocess.run([sys.executable, str(f)], input=data, capture_output=True, timeout=3)
        out.append(r.stdout.decode().strip())
    except Exception:
        pass
print("   ".join(filter(None, out)))
