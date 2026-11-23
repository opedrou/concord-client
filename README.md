# Concord Client

Cliente desktop nativo do [Concord](https://github.com/opedrou/concord), em Rust, para Windows e
Linux. Entra nas mesmas salas do site e aponta para qualquer servidor Concord: o endereço é
escolhido na primeira abertura.

**Versão atual: v0.1.20.** Baixe na [última release](https://github.com/opedrou/concord-client/releases/latest).

## O que já faz

- **Voz:** microfone com cancelamento de eco, limitador de voz automático ou manual,
  sensibilidade e ganho de entrada. Volume e botão de silenciar por pessoa, troca de dispositivo no meio da call, e
  o fone que cai volta sozinho quando reconecta. Avisa quem não está te ouvindo, call sendo
  gravada e conexão instável. Em Configurações › Voz dá pra testar a saída e ouvir a própria
  voz sem entrar numa call. Atalhos: Ctrl+Shift+M liga e desliga o mic, Ctrl+Shift+D o fone.
- **Transmitir a tela:** tela inteira ou janela, com o som do que está tocando, em estéreo. Dá
  pra trocar a tela e ligar ou desligar o áudio sem parar a transmissão, com prévia do que está
  saindo. A última tela fica lembrada e pode ser transmitida de novo sem o seletor.
- **Assistir** a transmissão dos outros, com o som, também de quem transmite pelo site, vendo
  quem mais está assistindo. Duplo clique ou `F` alterna a tela cheia.
- **Soundboard:** toca os sons do grupo pra todo mundo na call, junto com o site, com volume
  separado por pessoa. Dá pra subir e apagar sons pelo aplicativo.
- **Câmera dos outros** no tile de voz, no lugar da foto.
- **Chat de texto** nos canais de texto, em tempo real, com anexos pelo clipe, arrastando o
  arquivo ou colando imagem com Ctrl+V. Links abrem no navegador. O `@` tem autocompletar
  (setas e Tab) e a menção aparece destacada em roxo.
- **Lista de membros** à direita, com quem está online e quem está em cada canal de voz.
  Clicar na pessoa chama pra call, menciona no chat ou entra no canal de voz dela. Minimiza no
  botão de duas pessoas.
- **Notificações:** aviso do sistema com som quando alguém te chama pra call (botão "Chamar
  pessoas", no app ou no site) ou te marca com `@`. Clicar no aviso abre a call ou o chat.
- **Fica na bandeja:** o X fecha a janela e o Concord segue recebendo avisos. Ele também pode
  abrir junto com o computador, em segundo plano. As duas opções ficam em Configurações ›
  Notificações. O menu da bandeja liga e desliga o mic, sai da call e silencia os avisos por
  1 h.
- **Sons** de entrar, sair, começar a transmitir e ligar ou desligar o mic e o fone.
- **Atualiza sozinho:** confere versão nova a cada 15 min, e a seta verde reinicia na nova. O
  arquivo baixado é conferido pelo SHA-256 da release, e se a versão nova não abrir o app volta
  pra anterior. Em Configurações › Sobre dá pra procurar atualização na hora.

Ainda só no site: enviar a própria câmera e renomear ou cortar os sons da soundboard.

## Instalar

**Windows 11** (o 10 não foi testado): baixe `concord-client-windows-x86_64.exe` e abra. Ele se instala em
`%LOCALAPPDATA%\Programs\Concord`, cria o atalho no Menu Iniciar e na Área de Trabalho e aparece
em "Aplicativos instalados", que é por onde se desinstala; o desinstalar pergunta se apaga também
a config e o login salvo. O arquivo não é assinado: se o SmartScreen bloquear,
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
ícone, e pergunta se apaga também a config, o log (`~/.config/concord`) e o login salvo.

## Limites conhecidos

- No Linux, o monitor não é mantido aceso enquanto você assiste uma transmissão.
- No Windows, o vídeo da transmissão é codificado na CPU (VP8).
- No chat, selecionar e copiar funciona dentro de uma mensagem, não pegando várias de uma vez.
- O chat manda a foto original; o site encolhe antes de enviar.
- O aviso só chega com o Concord aberto, mesmo que só na bandeja. Quem estava offline perde o
  chamado.
- No Linux, a bandeja precisa da extensão AppIndicator, que já vem no Ubuntu. Sem ela, o X
  fecha o app.
- No Linux com Wayland, arrastar arquivo pra janela não anexa. O clipe funciona.
- Cada som da soundboard toca por até 2 min, e o modo foco do site não a cala no aplicativo.

## Novidades da v0.1.20

### Novo
- **Soundboard:** o botão de música na barra da call abre os sons do grupo. Clicar toca pra todo
  mundo, no aplicativo e no site, e "Parar" corta o som pra todos. Um aviso mostra quem tocou o
  quê.
- **Subir e apagar sons:** em Configurações › Soundboard. Aceita mp3, ogg, wav, flac ou m4a, até
  1 MB. Apagar aparece nos sons que você subiu, e em todos para administradores.
- **Volume da soundboard por pessoa:** no painel que abre ao clicar na pessoa, separado do volume
  da voz. Em zero, os sons dela não tocam pra você.

### Corrigido
- **Selo LIVE na barra lateral:** o botão de silenciar ficava por cima dele.
- **"Parar" da transmissão assistida:** não tinha fundo e sumia em cima do vídeo.

### Interno
- Dependência nova `symphonia`, que decodifica os sons.

### Limites conhecidos
- Renomear e cortar os sons da soundboard continuam só no site.
- Cada som toca por até 2 min, contados depois do corte.
- O modo foco do site não cala a soundboard no aplicativo.

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
