# Concord Client

Cliente desktop nativo do [Concord](https://github.com/opedrou/concord), em Rust, para Windows e
Linux. Entra nas mesmas salas do site e aponta para qualquer servidor Concord: o endereço é
escolhido na primeira abertura.

**Versão atual: v0.1.19.** Baixe na [última release](https://github.com/opedrou/concord-client/releases/latest).

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
- No Linux com Wayland, arrastar arquivo pra janela não anexa. O clipe funciona.

## Novidades da v0.1.19

### Novo
- **Transmitir de novo a mesma tela:** o painel de transmitir lembra a última tela ou janela e
  oferece "Transmitir de novo", sem passar pelo seletor. "Escolher outra" abre o seletor. No
  Linux, se a permissão do sistema venceu, ele pergunta de novo.
- **Tela inicial com quem está em voz:** os canais de voz com gente aparecem com os rostos e
  um Entrar, e há um atalho pra voltar ao último canal.
- **Atalhos:** Ctrl+Shift+M liga e desliga o mic, Ctrl+Shift+D o fone, e `F` alterna a tela
  cheia da transmissão assistida. Os atalhos não disparam enquanto você digita.
- **Na call:** a barra de controles fica sempre visível na grade. Clicar na pessoa abre o
  volume, agora com a porcentagem. Duplo clique na transmissão liga e desliga a tela cheia, e
  o quadro dela ganhou os botões de tela cheia e de mudo.
- **Barra lateral:** bloco "Em call" com o tempo de call e o botão de sair, pra sair com o chat
  aberto. Mudo por pessoa na lista da call e contador roxo de menções em cada canal de texto.
- **Membros:** clicar na pessoa abre um menu com "Chamar pra call", "Mencionar" e entrar no
  canal de voz em que ela está.
- **Chat:** links viram botões que abrem no navegador. Arrastar arquivo pra janela e colar
  imagem com Ctrl+V anexam. Botão "Novas mensagens" com a lista rolada pra cima, datas com
  Hoje e Ontem e ícones no anexar e no apagar.
- **Bandeja e avisos:** o menu da bandeja liga e desliga o mic, sai da call e tem "Não
  incomodar por 1 h". Menção com a janela fora de foco põe um ponto vermelho no ícone. O aviso
  de chamado tem os botões Entrar e Agora não.
- **Voz:** som curto ao ligar e desligar o mic e o fone, que toca mesmo com o fone desligado.
  Em Configurações › Voz: botão Testar na saída, "Ouvir minha voz" com a redução de ruído e o
  ganho aplicados, volume geral em porcentagem e botão Padrão nos controles.
- **Login e conexão:** botão pra mostrar a senha, contagem regressiva no bloqueio por
  tentativas demais e na tela Offline, e Cancelar quando a conexão demora.

### Corrigido
- **Sair e Trocar servidor perguntam antes** e avisam quando a call vai cair junto. Um clique
  perdido apagava o login.
- **Tentar de novo** onde antes era preciso sair e entrar: chat que não carregou, transmissão
  que não abriu e call que não reconectou.

### Interno
- Dependência nova `arboard`, só pra ler imagem da área de transferência.
- A cópia do `webrtc-sys` pede ao portal do Linux uma permissão que vale entre execuções, pra
  transmitir de novo sem o seletor.

### Limites conhecidos
- No Linux com Wayland, arrastar arquivo pra janela não anexa. O clipe funciona.

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
