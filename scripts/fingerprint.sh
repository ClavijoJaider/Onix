#!/bin/bash
# Uso: source fingerprint.sh; fingerprint <raiz>   (o ejecutar: fingerprint.sh <raiz>)
fingerprint() {
  local root="$1"
  {
    find "$root/crates" -path '*/target' -prune -o -type f -exec stat -f '%N %m %z' {} + 2>/dev/null
    stat -f '%N %m %z' "$root/Cargo.toml" "$root/Cargo.lock" 2>/dev/null
  } | LC_ALL=C sort | shasum | cut -d' ' -f1
}
if [ "${BASH_SOURCE[0]}" = "$0" ]; then fingerprint "${1:-$(cd "$(dirname "$0")/.." && pwd)}"; fi
