# Concord Client

Cliente desktop nativo do [Concord](https://github.com/opedrou/concord), em Rust, para Windows e
Linux. Entra nas mesmas salas do site e aponta para qualquer servidor Concord: o endereço é
escolhido na primeira abertura.

**Versão atual: v0.1.9.** Baixe na [última release](https://github.com/opedrou/concord-client/releases/latest).

## O que já faz

- **Voz:** microfone com cancelamento de eco, limitador de voz automático ou manual e
  sensibilidade de entrada. Volume por pessoa e troca de dispositivo no meio da call, e o fone
  que cai volta sozinho quando reconecta.
- **Transmitir a tela:** tela inteira ou janela, com o som do que está tocando, em estéreo. Dá
  pra trocar a tela e ligar ou desligar o áudio sem parar a transmissão, com prévia do que está
  saindo.
- **Assistir** a transmissão dos outros, como no site, vendo quem mais está assistindo.
- **Câmera dos outros** no tile de voz, no lugar da foto.
- **Sons** de entrar, sair e começar a transmitir.
- **Atualiza sozinho:** confere versão nova a cada 15 min, e a seta verde reinicia na nova. O
  arquivo baixado é conferido pelo SHA-256 da release, e se a versão nova não abrir o app volta
  pra anterior. Em Configurações › Sobre dá pra procurar atualização na hora.

Ainda só no site: soundboard, chat de texto e enviar a própria câmera.

## Instalar

**Windows 11** (o 10 não foi testado): baixe `concord-client-windows-x86_64.exe` e abra. Ele se instala em
`%LOCALAPPDATA%\Programs\Concord`, cria o atalho no Menu Iniciar e aparece em "Aplicativos
instalados", que é por onde se desinstala. O arquivo não é assinado: se o SmartScreen bloquear,
clique em "Mais informações" e depois em "Executar assim mesmo".

**Linux (Ubuntu 24.04 ou mais novo, Debian 13 ou mais novo, x86_64):**

```bash
curl -fsSL https://raw.githubusercontent.com/opedrou/concord-client/main/release/install.sh | sh
```

O script baixa a última release, confere o SHA-256 contra o do GitHub, instala em
`~/.local/bin/concord-client` e põe o Concord no menu de aplicativos. Precisa de `curl` e
`sha256sum`.

À mão, se preferir:

```bash
mkdir -p ~/.local/bin
mv ~/Downloads/concord-client-linux-x86_64 ~/.local/bin/concord-client
chmod +x ~/.local/bin/concord-client
~/.local/bin/concord-client
```

Na primeira abertura ele entra no menu de aplicativos. Precisa de PipeWire, e a transmissão usa
o seletor de tela do sistema (xdg-desktop-portal).

**Desinstalar no Linux:** `concord-client --uninstall`. Tira o binário, a entrada do menu e o
ícone. A config e o log em `~/.config/concord` ficam; apague a pasta à mão pra limpar tudo.

## Limites conhecidos

- No Linux, o monitor não é mantido aceso enquanto você assiste uma transmissão.
- No Windows, o vídeo da transmissão é codificado na CPU (VP8).

## Novidades da v0.1.9

### Novo
- **Câmera dos outros** aparece no tile de voz, no lugar da foto, como no site. Câmera desligada
  volta pra foto.
- **Quem está assistindo:** cada transmissão mostra até 3 avatares de quem assiste, e "+N" pro
  resto. O site também passa a mostrar quem assiste pelo app.
- **Versão nova que não abre volta pra anterior.** O app testa se a versão baixada abre neste
  computador antes de trocar. Se ela cair em duas aberturas seguidas, a terceira volta pra
  anterior, e aquela versão não é baixada de novo.
- **Procurar atualização** em Configurações › Sobre.
- **Linux:** instalação de uma linha e `concord-client --uninstall` (veja "Instalar").

### Corrigido
- **Som da transmissão chega estéreo** pra quem assiste. Antes chegava mono.

### Interno
- Cópia do SDK livekit 0.10.1 em `vendor/livekit`, com o patch que publica o áudio estéreo.
- Bancadas contra o Chrome pra câmera, pra quem assiste e pro estéreo (esquerda e direita
  medidas separadas).

### Limites conhecidos
- Nada desta versão foi testado num Windows de verdade, só compilado.
- A câmera vem numa resolução só, 640x360, qualquer que seja o tamanho do tile.
- Quem atualiza da v0.1.8 troca pra esta sem o teste da versão baixada, que só existe a partir
  desta.

## Compilar

Precisa de Rust (a versão fica fixa em `rust-toolchain.toml`) e clang 21 ou mais novo no Linux.

```bash
cargo run --release
```

Os binários de Windows e Linux iguais aos da release saem de um container Docker:

```bash
release/release.sh --build    # vão pra target/docker/dist/
```

## Testes

```bash
cargo test

# Contra um servidor de verdade
CONCORD_URL=... CONCORD_USER=... CONCORD_PASS=... cargo test -- --ignored servidor

# Cofre do sistema
cargo test -- --ignored cofre

# Gera snapshots/*.png das telas, sem abrir janela
ICED_TEST_BACKEND=tiny-skia cargo test -- --ignored prints
```

## Licença

MIT. As fontes em `assets/fonts` são instâncias estáticas de Bricolage Grotesque e Hanken Grotesk,
sob a SIL Open Font License.
