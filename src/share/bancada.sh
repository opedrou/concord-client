#!/bin/bash
# Publica em `spike-cliente` com fonte sintética, sem portal, e mede o que o Chrome recebe, uma
# linha por qualidade. Precisa do `npm run dev` do site. Uso, da raiz do cliente:
#   CONCORD_URL=http://localhost:3001 OUT=/pasta/temporaria src/share/bancada.sh [qualidades]
SITE="${CONCORD_SITE:?pasta do app do site}"
WEB="${CONCORD_BANCADA:?pasta da bancada web}"
export LIBCLANG_PATH=${LIBCLANG_PATH:-/usr/lib/llvm-21/lib}
export CONCORD_ROOM=spike-cliente RUST_LOG=error
export CONCORD_USER="$(grep -E '^ADMIN_USERNAME=' "$SITE/.env.local" | cut -d= -f2-)"
export CONCORD_PASS="$(grep -E '^ADMIN_PASSWORD=' "$SITE/.env.local" | cut -d= -f2-)"
BIN=$(cargo test --release --no-run 2>&1 | sed -n 's/.*Executable .*(\(.*\))/\1/p' | head -1)
[ -x "$BIN" ] || { echo "sem binário de teste"; exit 1; }
engines() { cat /proc/$1/fdinfo/* 2>/dev/null | awk '/^drm-pdev:/ {d=$2} /^drm-engine-/ && $2>0 {s[d" "$1]+=$2} END {for (k in s) print k, s[k]}' | sort; }
for q in ${*:-economica media alta maxima}; do
  log="$OUT/$q.log"
  SEGUNDOS=35 QUALIDADE=$q "$BIN" bancada_tela_qualidade --ignored --nocapture >"$log" 2>&1 &
  pub=$!
  until grep -q PUBLICADO "$log" || ! kill -0 $pub 2>/dev/null; do sleep 0.2; done
  node "$WEB/assiste.mjs" >"$OUT/$q-web.log" 2>&1 &
  web=$!
  sleep 9
  e1=$(engines $pub)
  cpu=$(bash "$WEB/mede.sh" $pub 10)
  e2=$(engines $pub)
  wait $web
  cp "$OUT/assiste.png" "$OUT/$q.png"
  wait $pub
  echo "== $q (saída $?): $cpu"
  grep -E 'VIDEO|CLICOU' "$OUT/$q-web.log" | cut -c1-150
  grep ENVIADO "$log" | sed -n 3,5p
  echo "gpu antes:  $(echo $e1)"
  echo "gpu depois: $(echo $e2)"
done
