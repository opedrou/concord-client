# Concord Client

Cliente desktop nativo do [Concord](https://github.com/opedrou/concord), em Rust, para Windows e
Linux. Entra nas mesmas salas do site e aponta para qualquer servidor Concord: o endereço é
escolhido na primeira abertura.

**Versão atual: v0.1.17.** Baixe na [última release](https://github.com/opedrou/concord-client/releases/latest).

## O que já faz

- **Voz:** microfone com cancelamento de eco, limitador de voz automático ou manual,
  sensibilidade e ganho de entrada. Volume por pessoa e troca de dispositivo no meio da call, e
  o fone que cai volta sozinho quando reconecta. Avisa quem não está te ouvindo, call sendo
  gravada e conexão instável.
- **Transmitir a tela:** tela inteira ou janela, com o som do que está tocando, em estéreo. Dá
  pra trocar a tela e ligar ou desligar o áudio sem parar a transmissão, com prévia do que está
  saindo.
- **Assistir** a transmissão dos outros, com o som, também de quem transmite pelo site, vendo
  quem mais está assistindo.
- **Câmera dos outros** no tile de voz, no lugar da foto.
- **Chat de texto** nos canais de texto, em tempo real, com anexos. O `@` tem autocompletar
  (setas e Tab) e a menção aparece destacada em roxo.
- **Lista de membros** à direita, com quem está online e quem está em cada canal de voz.
  Minimiza no botão de duas pessoas.
- **Notificações:** aviso do sistema com som quando alguém te chama pra call (botão "Chamar
  pessoas", no app ou no site) ou te marca com `@`. Clicar no aviso abre a call ou o chat.
- **Fica na bandeja:** o X fecha a janela e o Concord segue recebendo avisos. Ele também pode
  abrir junto com o computador, em segundo plano. As duas opções ficam em Configurações ›
  Notificações.
- **Sons** de entrar, sair e começar a transmitir.
- **Atualiza sozinho:** confere versão nova a cada 15 min, e a seta verde reinicia na nova. O
  arquivo baixado é conferido pelo SHA-256 da release, e se a versão nova não abrir o app volta
  pra anterior. Em Configurações › Sobre dá pra procurar atualização na hora.

Ainda só no site: soundboard e enviar a própria câmera.

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

## Novidades da v0.1.17

### Novo
- **Notificações:** quando alguém te chama pra uma call ou te marca com `@` no chat, chega um
  aviso do sistema com som, mesmo com a janela fechada. Clicar no aviso abre a call ou o chat.
  Dá pra desligar em Configurações › Notificações.
- **Bandeja e iniciar com o computador:** o X fecha a janela e o Concord segue na bandeja, com
  "Abrir Concord" e "Sair". Ele pode abrir junto com o sistema, em segundo plano, só escutando
  os avisos. Abrir de novo traz a janela que já está rodando, em vez de abrir outro.
- **Chamar pessoas:** botão na call pra escolher quem chamar, como o do site.
- **Lista de membros:** painel à direita com Online e Offline, quem está em voz e o botão de
  minimizar.
- **Menção no chat:** autocompletar do `@` com setas, Tab e Enter, e a menção destacada em roxo
  na mensagem e na caixa de digitar.
- **Som de abertura:** toca na primeira vez do dia que a janela abre. Abrir em segundo plano
  não toca.

### Removido
- **Chamado pelo WhatsApp:** o servidor não manda mais o chamado pro grupo do WhatsApp. Ele
  chega como aviso no Concord.

### Interno
- Uma instância só por usuário, por uma porta local: a segunda abertura passa o pedido pra
  primeira e sai.
- Bancada ponta a ponta das notificações contra um servidor local.

### Limites conhecidos
- Precisa do servidor atualizado: com o servidor antigo não chega aviso e a lista mostra todo
  mundo offline, menos quem está em voz.
- Iniciar com o computador não foi testado num boot de verdade, só com o mesmo comando que o
  sistema roda.

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
