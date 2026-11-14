#!/bin/sh
# Instala o Concord no Linux: baixa a última release, confere o SHA-256 e põe em
# ~/.local/bin/concord-client. Uso:
#   curl -fsSL https://raw.githubusercontent.com/opedrou/concord-client/main/release/install.sh | sh
# CONCORD_BIN=/caminho/binario pula o download e a conferência (teste).
set -eu

REPO=opedrou/concord-client
ASSET=concord-client-linux-x86_64
DEST_DIR="$HOME/.local/bin"
DEST="$DEST_DIR/concord-client"

die() {
    echo "Erro: $*" >&2
    exit 1
}

[ "$(uname -s)" = Linux ] || die "este script é só pra Linux."
[ "$(uname -m)" = x86_64 ] || die "só existe build x86_64; esta máquina é $(uname -m)."
for cmd in curl sha256sum; do
    command -v "$cmd" >/dev/null 2>&1 || die "falta o '$cmd'. Instale (ex.: sudo apt install curl coreutils) e rode de novo."
done

mkdir -p "$DEST_DIR"
# No mesmo diretório do destino, pro `mv` final ser uma troca atômica.
tmp=$(mktemp "$DEST_DIR/.concord-client.XXXXXX")
trap 'rm -f "$tmp"' EXIT INT TERM

if [ -n "${CONCORD_BIN:-}" ]; then
    cp "$CONCORD_BIN" "$tmp"
else
    echo "Baixando o Concord..."
    curl -fsSL "https://github.com/$REPO/releases/latest/download/$ASSET" -o "$tmp" ||
        die "não consegui baixar a release."

    # O JSON da API traz "name" e depois "digest" de cada asset. Sem jq: separa por vírgula e
    # pega o primeiro "digest" depois do nome do nosso asset.
    esperado=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
        tr ',' '\n' |
        awk -v nome="\"$ASSET\"" '
            index($0, "\"name\"") && index($0, nome) { achou = 1 }
            achou && /"digest"/ {
                if (match($0, /sha256:[0-9a-f]+/)) print substr($0, RSTART + 7, RLENGTH - 7)
                exit
            }') || true
    [ -n "$esperado" ] || die "não achei o SHA-256 da release na API do GitHub (limite de requisições?). Nada foi instalado."
    obtido=$(sha256sum "$tmp" | cut -d' ' -f1)
    if [ "$obtido" != "$esperado" ]; then
        die "SHA-256 não bate (esperado $esperado, veio $obtido). Arquivo apagado, nada foi instalado."
    fi
    echo "SHA-256 confere."
fi

chmod +x "$tmp"
mv -f "$tmp" "$DEST"
trap - EXIT INT TERM
echo "Instalado em $DEST"

# `--register` põe a entrada no menu e sai, sem abrir janela.
"$DEST" --register || echo "Aviso: não consegui registrar a entrada no menu; ela aparece na primeira abertura." >&2

case ":$PATH:" in
*":$DEST_DIR:"*) ;;
*) echo "Aviso: $DEST_DIR não está no PATH. Abra o Concord pelo menu ou rode $DEST." >&2 ;;
esac
echo "Pronto. Abra pelo menu de aplicativos ou rode: concord-client"
echo "Pra desinstalar: concord-client --uninstall"
