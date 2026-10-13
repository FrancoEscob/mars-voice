#!/usr/bin/env bash
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
with_service=false
case "${1:-}" in
  '') ;;
  --with-service) with_service=true ;;
  -h|--help)
    printf '%s\n' 'Uso: scripts/install-user.sh [--with-service]' \
      'Instala el binario ya compilado y el lanzador para el usuario actual.' \
      'DICTA_PROFILE=debug usa target/debug; por defecto usa release.' \
      '--with-service copia una unidad systemd user sin activarla.'
    exit 0 ;;
  *) printf '%s\n' 'Opción desconocida. Usá --help.' >&2; exit 2 ;;
esac
[[ $# -le 1 ]] || { printf '%s\n' 'Demasiados argumentos.' >&2; exit 2; }
profile="${DICTA_PROFILE:-release}"
case "$profile" in release|debug) ;; *) printf '%s\n' 'DICTA_PROFILE debe ser release o debug.' >&2; exit 2 ;; esac
source_binary="$project_dir/target/$profile/dicta"
[[ -x "$source_binary" ]] || { printf 'Primero compilá con DICTA_PROFILE=%s scripts/build-local.sh\n' "$profile" >&2; exit 1; }
command -v python3 >/dev/null || { printf '%s\n' 'Se necesita python3 para generar el lanzador con rutas escapadas.' >&2; exit 1; }
user_bin="$HOME/.local/bin"
user_data="${XDG_DATA_HOME:-$HOME/.local/share}"
install -d "$user_bin" "$user_data/applications"
# Use rename so updating an executable that is currently running remains safe.
install -m 0755 "$source_binary" "$user_bin/dicta.new"
mv -f "$user_bin/dicta.new" "$user_bin/dicta"
python3 - "$project_dir/packaging/io.github.francoescob.Dicta.desktop" "$user_data/applications/io.github.francoescob.Dicta.desktop" "$user_bin/dicta" <<'PY'
from pathlib import Path
import sys
source, destination, executable = sys.argv[1:]
# Desktop Entry exec quoting is separate from shell quoting; no shell is used.
escaped = executable.replace('\\', '\\\\').replace('"', '\\"').replace('`', '\\`').replace('$', '\\$')
# Literal percent must be escaped to prevent desktop field-code expansion.
escaped = escaped.replace('%', '%%')
text = Path(source).read_text().replace('@EXEC@', '"' + escaped + '"').replace('@TRYEXEC@', executable)
Path(destination).write_text(text)
Path(destination).chmod(0o644)
PY
icon="$project_dir/design/mars-voice/icons/mars-voice-launcher.svg"
[[ -f "$icon" ]] || icon="$project_dir/crates/dicta-ui/assets/marks/dicta-mark.svg"
if [[ -f "$icon" ]]; then
  install -d "$user_data/icons/hicolor/scalable/apps"
  install -m 0644 "$icon" "$user_data/icons/hicolor/scalable/apps/io.github.francoescob.Dicta.svg"
fi
if $with_service; then
  user_config="${XDG_CONFIG_HOME:-$HOME/.config}"
  install -d "$user_config/systemd/user"
  install -m 0644 "$project_dir/packaging/dicta.service" "$user_config/systemd/user/dicta.service"
  printf '%s\n' 'Unidad copiada, sin activar. Para activarla conscientemente dentro de la sesión gráfica:' \
    '  systemctl --user daemon-reload' \
    '  systemctl --user enable --now dicta.service'
fi
if command -v update-desktop-database >/dev/null; then
  update-desktop-database "$user_data/applications" || true
fi
printf 'Mars voice instalado en %s/dicta\n' "$user_bin"
printf 'Lanzador: %s/applications/io.github.francoescob.Dicta.desktop\n' "$user_data"
printf '%s\n' 'La aplicación no se inició. No se cambió Hyprland ni se habilitó inicio automático.'
