#!/bin/bash
# Roda os testes de bancada do áudio num PipeWire próprio, sem ALSA nem Bluetooth: daqui
# não dá pra alcançar a caixa nem o mic de verdade. Uso: src/audio/bancada.sh [filtro]
RAIZ=$(mktemp -d "$XDG_RUNTIME_DIR/concord-bancada.XXXXXX") || exit 1
sobe() {
  # HOME próprio ignora a config do usuário; sem D-Bus não há como chegar no Bluetooth.
  env -i PATH="$PATH" HOME="$RAIZ" XDG_RUNTIME_DIR="$RAIZ" XDG_CONFIG_HOME="$RAIZ/cfg" \
    XDG_STATE_HOME="$RAIZ/state" XDG_DATA_HOME="$RAIZ/data" \
    DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent DBUS_SYSTEM_BUS_ADDRESS=unix:path=/nonexistent \
    "$@" >>"$RAIZ/log" 2>&1
}
limpa() {
  touch "$RAIZ/fim"
  # $WP e $PW são o subshell do `sobe`: o daemon é filho dele.
  kill "$(cat "$RAIZ/pulse/pid" 2>/dev/null)" $(pgrep -P "$WP") $(pgrep -P "$PW") "$WP" "$PW" 2>/dev/null
  wait 2>/dev/null
  rm -rf "$RAIZ"
}
trap limpa EXIT
trap 'exit 130' INT TERM
sobe pipewire &
PW=$!
sleep 1
# Volta sozinho: o teste derruba o servidor de propósito.
(until [ -e "$RAIZ/fim" ]; do sobe pipewire-pulse; sleep 0.2; done) &
# O perfil `policy` não carrega nada de hardware.
sobe wireplumber --profile=policy &
WP=$!
sleep 2
# PIPEWIRE_RUNTIME_DIR leva `pw-dump`, `pw-link` e `pw-record` pro PipeWire daqui, não pro
# da sessão.
export PULSE_SERVER="unix:$RAIZ/pulse/native" BANCADA_RAIZ="$RAIZ" PIPEWIRE_RUNTIME_DIR="$RAIZ"
cargo test "${1:-bancada}" -- --ignored --nocapture --test-threads=1
