#!/bin/bash
# Chat de ponta a ponta no site local: cliente pela API e Chrome pelo site, nos dois sentidos,
# com o SSE do cliente derrubado no meio. Precisa do `npm run dev` do site e de um canal de
# texto. Uso, da raiz do cliente:
#   CONCORD_URL=http://localhost:3000 CONCORD_CANAL=teste-chat src/chat/bancada.sh
SITE="${CONCORD_SITE:?pasta do app do site}"
export CONCORD_BANCADA="${CONCORD_BANCADA:?pasta da bancada web}"
export CONCORD_URL="${CONCORD_URL:-http://localhost:3000}" CONCORD_CANAL="${CONCORD_CANAL:?slug do canal de texto}"
export LIBCLANG_PATH=${LIBCLANG_PATH:-/usr/lib/llvm-21/lib}
export CONCORD_USER="$(grep -E '^ADMIN_USERNAME=' "$SITE/.env.local" | cut -d= -f2-)"
export CONCORD_PASS="$(grep -E '^ADMIN_PASSWORD=' "$SITE/.env.local" | cut -d= -f2-)"
export OUT="${OUT:-$(mktemp -d)}"
echo "saída em $OUT"
nice -n 19 cargo test bancada_chat -- --ignored --nocapture
