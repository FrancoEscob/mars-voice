#!/usr/bin/env bash
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_dir"
profile="${DICTA_PROFILE:-release}"
case "$profile" in
  release) profile_args=(--release) ;;
  debug) profile_args=() ;;
  *) printf '%s\n' 'DICTA_PROFILE debe ser release o debug.' >&2; exit 2 ;;
esac
command -v cargo >/dev/null || { printf '%s\n' 'Falta cargo. Instalá el toolchain Rust para compilar.' >&2; exit 1; }
command -v pkg-config >/dev/null || { printf '%s\n' 'Falta pkg-config.' >&2; exit 1; }
command -v c++ >/dev/null || command -v clang++ >/dev/null || { printf '%s\n' 'Falta un compilador C++ (g++ o clang++): lo usa la velocidad de lectura.' >&2; exit 1; }
pkg-config --exists libpipewire-0.3 || { printf '%s\n' 'Faltan las bibliotecas/headers de desarrollo de PipeWire.' >&2; exit 1; }
if [[ -z "${CLANG_PATH:-}" ]] && ! command -v clang >/dev/null; then
  # Algunas distros instalan clang solo con sufijo de versión (p. ej. Arch con
  # llvm21: /usr/lib/llvm21/bin/clang-21, Ubuntu: /usr/lib/llvm-18/bin/clang).
  found=""
  for candidate in /usr/lib/llvm*/bin/clang-[0-9]* /usr/lib/llvm-*/bin/clang /usr/bin/clang-[0-9]*; do
    [[ -x "$candidate" ]] && found="$candidate"
  done
  if [[ -n "$found" ]]; then
    export CLANG_PATH="$found"
  else
    printf '%s\n' 'No se encontró clang. Instalalo o definí CLANG_PATH con su ruta.' >&2
    exit 1
  fi
fi
cargo build --locked -p dicta "${profile_args[@]}"
printf 'Ejecutable listo: %s/target/%s/dicta\n' "$project_dir" "$profile"
printf '%s\n' 'No se instaló ni ejecutó la aplicación.'
