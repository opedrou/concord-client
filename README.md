# Concord Client

Cliente desktop nativo do [Concord](https://github.com/opedrou/concord), em Rust, para Windows e
Linux. Entra nas mesmas salas do site e aponta para qualquer servidor Concord: o endereço é
escolhido na primeira abertura.

**Versão atual: v0.1.8.** Baixe na [última release](https://github.com/opedrou/concord-client/releases/latest).

## O que já faz

- **Voz:** microfone com cancelamento de eco, limitador de voz automático ou manual e
  sensibilidade de entrada. Volume por pessoa e troca de dispositivo no meio da call, e o fone
  que cai volta sozinho quando reconecta.
- **Transmitir a tela:** tela inteira ou janela, com o som do que está tocando. Dá pra trocar a
  tela e ligar ou desligar o áudio sem parar a transmissão, com prévia do que está saindo.
- **Assistir** a transmissão dos outros, como no site.
- **Sons** de entrar, sair e começar a transmitir.
- **Atualiza sozinho:** confere versão nova a cada 15 min, e a seta verde reinicia na nova. O
  arquivo baixado é conferido pelo SHA-256 da release.

Ainda só no site: soundboard e chat de texto.

## Instalar

**Windows 11** (o 10 não foi testado): baixe `concord-client-windows-x86_64.exe` e abra. Ele se instala em
`%LOCALAPPDATA%\Programs\Concord`, cria o atalho no Menu Iniciar e aparece em "Aplicativos
instalados", que é por onde se desinstala. O arquivo não é assinado: se o SmartScreen bloquear,
clique em "Mais informações" e depois em "Executar assim mesmo".

**Linux (Ubuntu 24.04 ou mais novo, Debian 13 ou mais novo, x86_64):**

```bash
mkdir -p ~/.local/bin
mv ~/Downloads/concord-client-linux-x86_64 ~/.local/bin/concord-client
chmod +x ~/.local/bin/concord-client
~/.local/bin/concord-client
```

Na primeira abertura ele entra no menu de aplicativos. Precisa de PipeWire, e a transmissão usa
o seletor de tela do sistema (xdg-desktop-portal).

## Limites conhecidos

- O som da transmissão chega mono.
- No Linux, o monitor não é mantido aceso enquanto você assiste uma transmissão.
- No Windows, o vídeo da transmissão é codificado na CPU (VP8).

## Novidades da v0.1.8

### Novo
- **Log em arquivo** pra investigar problema de áudio: `~/.config/concord/concord.log` no Linux,
  `%APPDATA%\Concord\concord.log` no Windows. Guarda sala, faixas, dispositivos, mudo e surdo, e
  recomeça ao passar de 1 MB.

### Corrigido
- **Fone que cai ou que liga depois de entrar na call:** a voz ia pro dispositivo padrão do
  sistema e ficava lá até o fim. Agora o app confere a cada segundo e volta pro fone escolhido
  assim que ele reaparece, na saída e no microfone.
- **Áudio travado quando a saída some** com um som tocando: o app espera no máximo 3 s e segue.

### Interno
- Bancada que confere se a call ouve o Chrome em 22 medidas: entrar e sair, assistir, prévia e
  quedas de rede.

### Limites conhecidos
- No Windows, a volta pro fone que reconecta ainda não foi testada com um fone de verdade.

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
