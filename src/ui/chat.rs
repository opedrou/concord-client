//! Canal de texto aberto, no lugar do palco: a call continua tocando por trás. A lógica
//! fica em `crate::chat`.
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::Duration;

use futures_util::{Stream, StreamExt};

use iced::keyboard::{Key, key::Named};
use iced::widget::operation::{self, AbsoluteOffset};
use iced::widget::text_editor::{Action, Binding, KeyPress, Motion};
use iced::widget::{
    Column, button, column, container, mouse_area, row, scrollable, space, text, text_editor,
};
use iced::{
    Alignment, Background, Border, Color, Element, Length, Padding, Subscription, Task, Theme,
    border,
};

use super::sidebar::{CLOSE, HASH, icon};
use super::theme::{self, BG, BG2, BORDER, BORDER2, DANGER, FG, FG2, FG3, HOVER, IDLE};
use super::{Main, Msg as App, avatar};
use crate::api::{Attachment, Channel, Client, Error, Kind, Message, Page};
use crate::chat::{self as logic, Live, Timeline, Window};

const LISTA: &str = "chat-lista";
const COMPOSER: &str = "chat-composer";
const ROSTO: f32 = 40.0;
/// Busca do que perdeu que falhou com o tempo real de pé: tenta de novo depois disso.
const DE_NOVO: Duration = Duration::from_secs(3);
const TEXTO: f32 = 14.8;
/// Seleção bem visível no fundo escuro: o `SOFT` do tema some aqui.
const SELECAO: Color = Color::from_rgba8(157, 78, 221, 0.45);

#[derive(Clone, Debug)]
pub enum Msg {
    /// Clique no canal na sidebar.
    Open,
    Close,
    Loaded(Result<Window, Error>),
    More,
    MoreLoaded(Result<Page, Error>),
    Draft(Action),
    /// Seleção no texto de uma mensagem.
    Select(i64, Action),
    Send,
    Sent(Result<Message, Error>),
    /// Mouse entrou (`true`) ou saiu de uma mensagem.
    Hover(i64, bool),
    /// Evento do SSE do canal.
    Live(Live),
    /// Tenta de novo a busca do que perdeu.
    Recover,
    /// Sessão vencida no meio: volta pro login.
    Expired,
}

pub struct Chat {
    pub channel: i64,
    name: String,
    timeline: Timeline,
    /// Texto de cada mensagem num editor só de leitura: é o que deixa selecionar e copiar.
    texts: HashMap<i64, text_editor::Content>,
    draft: text_editor::Content,
    loaded: bool,
    loading_more: bool,
    sending: bool,
    error: Option<String>,
    hover: Option<i64>,
    /// O tempo real caiu e está tentando voltar.
    lost: bool,
    live: bool,
}

impl Chat {
    pub fn open(client: &Client, channel: &Channel) -> (Self, Task<App>) {
        let chat = Self {
            channel: channel.id,
            name: channel.name.clone(),
            timeline: Timeline::default(),
            texts: HashMap::new(),
            draft: text_editor::Content::new(),
            loaded: false,
            loading_more: false,
            sending: false,
            error: None,
            hover: None,
            lost: false,
            live: false,
        };
        let task = Task::batch([chat.recover(client), operation::focus(COMPOSER)]);
        (chat, task)
    }

    /// SSE do canal enquanto o chat está aberto: fechou, a conexão cai junto.
    pub fn subscription(&self, client: &Client) -> Subscription<App> {
        Subscription::run_with(Feed(client.clone(), self.channel), feed)
    }

    /// Quem aparece na tela, pra baixar as fotos.
    pub fn authors(&self) -> impl Iterator<Item = &str> {
        self.timeline
            .messages
            .iter()
            .map(|m| m.author_name.as_str())
    }

    /// Primeira carga, ou o que perdeu depois da mais nova da tela.
    fn recover(&self, client: &Client) -> Task<App> {
        let (client, channel, asked) = (client.clone(), self.channel, self.timeline.newest());
        Task::future(async move {
            App::Chat(
                channel,
                Msg::Loaded(logic::recover(&client, channel, asked).await),
            )
        })
    }

    pub fn update(&mut self, msg: Msg, client: &Client) -> Task<App> {
        let channel = self.channel;
        match msg {
            Msg::Loaded(Ok(window)) => {
                self.loaded = true;
                self.error = None;
                self.timeline.recovered(window);
                self.sync();
            }
            Msg::Loaded(Err(e)) => {
                let task = self.failed(e, "Não foi possível carregar as mensagens.");
                if !self.live || e == Error::Unauthorized {
                    return task;
                }
                // Sem a busca, o que chegou com a conexão fora ficaria faltando até a próxima queda.
                return Task::future(async move {
                    tokio::time::sleep(DE_NOVO).await;
                    App::Chat(channel, Msg::Recover)
                });
            }
            Msg::Recover => return self.recover(client),
            // A cada volta, busca o que perdeu. A primeira cobre o que chegou entre a carga
            // inicial e o servidor começar a mandar os eventos.
            Msg::Live(Live::Connected) => {
                (self.live, self.lost) = (true, false);
                return self.recover(client);
            }
            Msg::Live(Live::Event(event)) => {
                self.timeline.apply(event);
                self.sync();
            }
            Msg::Live(Live::Lost) => (self.live, self.lost) = (false, true),
            Msg::Live(Live::Unauthorized) => return self.failed(Error::Unauthorized, ""),
            Msg::More => {
                if self.loading_more || !self.timeline.has_more {
                    return Task::none();
                }
                self.loading_more = true;
                let (client, before) = (client.clone(), self.timeline.oldest());
                return Task::future(async move {
                    let page = client.messages(channel, before, logic::PAGINA).await;
                    App::Chat(channel, Msg::MoreLoaded(page))
                });
            }
            Msg::MoreLoaded(result) => {
                self.loading_more = false;
                match result {
                    Ok(page) => {
                        self.timeline.older(page);
                        self.sync();
                    }
                    Err(e) => return self.failed(e, "Não foi possível carregar as mensagens."),
                }
            }
            Msg::Draft(action) => self.edit(action),
            Msg::Select(id, action) => {
                if let Some(content) = self.texts.get_mut(&id).filter(|_| !action.is_edit()) {
                    content.perform(action);
                }
            }
            Msg::Send => return self.send(client),
            Msg::Sent(result) => {
                self.sending = false;
                match result {
                    Ok(message) => {
                        self.draft = text_editor::Content::new();
                        self.timeline.insert(message);
                        self.sync();
                        return to_bottom();
                    }
                    Err(e) => return self.failed(e, "Não foi possível enviar a mensagem."),
                }
            }
            Msg::Hover(id, true) => self.hover = Some(id),
            Msg::Hover(id, false) => {
                if self.hover == Some(id) {
                    self.hover = None;
                }
            }
            Msg::Open | Msg::Close | Msg::Expired => {}
        }
        Task::none()
    }

    fn failed(&mut self, e: Error, what: &str) -> Task<App> {
        if e == Error::Unauthorized {
            return Task::done(App::Chat(self.channel, Msg::Expired));
        }
        self.error = Some(what.to_owned());
        Task::none()
    }

    /// Enquanto envia, o texto fica parado: a resposta limpa o campo.
    fn edit(&mut self, action: Action) {
        if self.sending && action.is_edit() {
            return;
        }
        let edit = action.is_edit();
        self.draft.perform(action);
        if !edit {
            return;
        }
        self.error = None;
        let text = self.draft.text();
        if !logic::fits(&text) {
            // Colou demais: corta no limite, como o `maxLength` da web.
            self.draft = text_editor::Content::with_text(&logic::cut(&text));
            self.draft.perform(Action::Move(Motion::DocumentEnd));
        }
    }

    fn send(&mut self, client: &Client) -> Task<App> {
        let text = self.draft.text();
        let content = text.trim();
        if self.sending || content.is_empty() || !logic::fits(content) {
            return Task::none();
        }
        self.sending = true;
        self.error = None;
        let (client, channel, content) = (client.clone(), self.channel, content.to_owned());
        Task::future(async move {
            App::Chat(
                channel,
                Msg::Sent(client.send_message(channel, &content).await),
            )
        })
    }

    /// Um editor por mensagem com texto, criado uma vez e solto quando ela sai.
    fn sync(&mut self) {
        let ids: HashSet<i64> = self.timeline.messages.iter().map(|m| m.id).collect();
        self.texts.retain(|id, _| ids.contains(id));
        for m in self
            .timeline
            .messages
            .iter()
            .filter(|m| !m.content.is_empty())
        {
            self.texts
                .entry(m.id)
                .or_insert_with(|| text_editor::Content::with_text(&m.content));
        }
    }
}

/// Chave da assinatura: o canal. O cliente vai junto, mas não muda enquanto o chat existe.
struct Feed(Client, i64);

impl Hash for Feed {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.1.hash(state);
    }
}

fn feed(Feed(client, channel): &Feed) -> impl Stream<Item = App> + use<> {
    let channel = *channel;
    logic::live(client.clone(), channel).map(move |live| App::Chat(channel, Msg::Live(live)))
}

/// Com a lista presa no pé, deslocamento zero é a mensagem mais nova.
fn to_bottom() -> Task<App> {
    operation::scroll_to(
        LISTA,
        AbsoluteOffset {
            x: None,
            y: Some(0.0),
        },
    )
}

pub fn view<'a>(m: &'a Main, chat: &'a Chat) -> Element<'a, App> {
    let channel = chat.channel;
    let close = button(container(icon(CLOSE, 16.0, FG2)).center(30))
        .padding(0)
        .style(theme::ghost)
        .on_press(App::Chat(channel, Msg::Close));
    let lost = chat
        .lost
        .then(|| text("Reconectando…").size(12.5).color(IDLE));
    let header = container(
        row![
            icon(HASH, 18.0, FG3),
            text(&chat.name).size(16.5).font(theme::SEMIBOLD),
            space().width(Length::Fill),
        ]
        .push(lost)
        .push(close)
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([10, 16])
    .width(Length::Fill);

    let empty = |label| -> Element<'a, App> {
        container(text(label).size(14).color(FG2))
            .center(Length::Fill)
            .into()
    };
    let body = if chat.timeline.messages.is_empty() {
        match (chat.loaded, &chat.error) {
            (false, None) => empty("Carregando mensagens…"),
            (false, Some(_)) => empty("Não foi possível carregar as mensagens."),
            (true, _) => empty("Nenhuma mensagem ainda."),
        }
    } else {
        list(m, chat)
    };
    let divider = container(space())
        .width(Length::Fill)
        .height(1)
        .style(theme::fill(BORDER));
    container(column![header, divider, body, composer(chat)])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::fill(BG))
        .into()
}

fn list<'a>(m: &'a Main, chat: &'a Chat) -> Element<'a, App> {
    let channel = chat.channel;
    let more = chat.timeline.has_more.then(|| {
        let label = if chat.loading_more {
            "Carregando…"
        } else {
            "Carregar mais"
        };
        container(
            button(text(label).size(13.5))
                .padding([7, 16])
                .style(theme::outline)
                .on_press_maybe((!chat.loading_more).then_some(App::Chat(channel, Msg::More))),
        )
        .center_x(Length::Fill)
        .padding(Padding {
            top: 4.0,
            bottom: 8.0,
            ..Padding::ZERO
        })
    });
    let rows = chat
        .timeline
        .messages
        .iter()
        .enumerate()
        .map(|(index, message)| message_row(m, chat, index, message));
    let items = Column::new().push(more).extend(rows).padding(Padding {
        top: 8.0,
        right: 14.0,
        bottom: 12.0,
        left: 10.0,
    });
    // Pouca mensagem fica embaixo, junto do campo, como no Discord.
    container(
        scrollable(items)
            .id(LISTA)
            .anchor_bottom()
            .style(theme::thin_scroll),
    )
    .width(Length::Fill)
    .align_bottom(Length::Fill)
    .into()
}

fn message_row<'a>(
    m: &'a Main,
    chat: &'a Chat,
    index: usize,
    message: &'a Message,
) -> Element<'a, App> {
    let (channel, id) = (chat.channel, message.id);
    let starts = chat.timeline.starts_group(index);
    let hovered = chat.hover == Some(id);

    let mut body = Column::new().spacing(4);
    if let Some(content) = chat.texts.get(&id) {
        let selectable = text_editor(content)
            .on_action(move |action| App::Chat(channel, Msg::Select(id, action)))
            .size(TEXTO)
            .padding(0)
            .style(read_only);
        body = body.push(selectable);
    }
    if let Some(attachment) = &message.attachment {
        body = body.push(attachment_chip(attachment));
    }

    let name = message.author_name.as_str();
    let gutter: Element<'a, App> = if starts {
        avatar::view(name, m.avatar(name), ROSTO)
    } else if hovered {
        text(clock(message.created_at))
            .size(10.5)
            .color(FG3)
            .center()
            .width(ROSTO)
            .into()
    } else {
        space().width(ROSTO).into()
    };
    let main: Element<'a, App> = if starts {
        let meta = row![
            text(name).size(15).font(theme::SEMIBOLD).color(FG),
            text(when(message.created_at)).size(11.5).color(FG3),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        column![meta, body].spacing(3).into()
    } else {
        body.into()
    };
    let line = row![container(gutter).width(ROSTO), main]
        .spacing(14)
        .align_y(Alignment::Start);
    let cell = container(line)
        .padding(if starts { [5, 8] } else { [1, 8] })
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: hovered.then_some(Background::Color(HOVER)),
            border: border::rounded(9),
            ..Default::default()
        });
    let cell = mouse_area(cell)
        .on_enter(App::Chat(channel, Msg::Hover(id, true)))
        .on_exit(App::Chat(channel, Msg::Hover(id, false)));
    if starts && index > 0 {
        column![space().height(10), cell].into()
    } else {
        cell.into()
    }
}

fn attachment_chip(attachment: &Attachment) -> Element<'_, App> {
    let kind = match attachment.kind {
        Kind::Image => "Imagem",
        Kind::Video => "Vídeo",
        Kind::Audio => "Áudio",
        Kind::File => "Arquivo",
    };
    let label = format!("{kind}: {} · {}", attachment.name, size(attachment.size()));
    container(text(label).size(13.5).color(FG2))
        .padding([8, 12])
        .style(chip)
        .into()
}

fn composer(chat: &Chat) -> Element<'_, App> {
    let channel = chat.channel;
    let editor = text_editor(&chat.draft)
        .id(COMPOSER)
        .placeholder(format!("Conversar em #{}", chat.name))
        .on_action(move |action| App::Chat(channel, Msg::Draft(action)))
        .key_binding(move |key| enter_sends(key, channel))
        .size(TEXTO)
        .padding([9, 0])
        .max_height(140)
        .style(read_only);
    let ready = !chat.sending && !chat.draft.text().trim().is_empty();
    let send = button(
        text(if chat.sending {
            "Enviando…"
        } else {
            "Enviar"
        })
        .size(14)
        .font(theme::SEMIBOLD),
    )
    .padding([8, 16])
    .style(theme::primary)
    .on_press_maybe(ready.then_some(App::Chat(channel, Msg::Send)));
    let card = container(row![editor, send].spacing(10).align_y(Alignment::End))
        .padding(Padding {
            top: 5.0,
            right: 6.0,
            bottom: 5.0,
            left: 14.0,
        })
        .style(|_| container::Style {
            background: Some(BG2.into()),
            border: Border {
                color: BORDER2,
                width: 1.0,
                radius: 14.0.into(),
            },
            ..Default::default()
        });
    let error = chat
        .error
        .as_deref()
        .map(|e| text(e).size(13.5).color(DANGER));
    column![]
        .push(error)
        .push(card)
        .spacing(6)
        .padding(Padding {
            top: 4.0,
            right: 16.0,
            bottom: 16.0,
            left: 16.0,
        })
        .into()
}

/// Enter envia; Shift+Enter quebra a linha, como na web.
fn enter_sends(key: KeyPress, channel: i64) -> Option<Binding<App>> {
    let focused = matches!(key.status, text_editor::Status::Focused { .. });
    match key.key.as_ref() {
        Key::Named(Named::Enter) if focused && !key.modifiers.shift() => {
            Some(Binding::Custom(App::Chat(channel, Msg::Send)))
        }
        _ => Binding::from_key_press(key),
    }
}

/// Editor sem borda nem fundo: parece texto, mas deixa selecionar.
fn read_only(_: &Theme, _: text_editor::Status) -> text_editor::Style {
    text_editor::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        placeholder: FG3,
        value: Color::from_rgb8(0xe8, 0xe8, 0xea),
        selection: SELECAO,
    }
}

fn chip(_: &Theme) -> container::Style {
    container::Style {
        background: Some(BG2.into()),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 10.0.into(),
        },
        ..Default::default()
    }
}

fn size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    match bytes as f64 {
        b if b < KB => format!("{bytes} B"),
        b if b < KB * KB => format!("{:.0} KB", b / KB),
        b => format!("{:.1} MB", b / (KB * KB)),
    }
}

/// Hora local da mensagem; de outro dia, com a data.
fn when(ms: i64) -> String {
    use chrono::{DateTime, Local};
    let Some(at) = DateTime::from_timestamp_millis(ms) else {
        return String::new();
    };
    let at = at.with_timezone(&Local);
    let format = if at.date_naive() == Local::now().date_naive() {
        "%H:%M"
    } else {
        "%d/%m/%Y %H:%M"
    };
    at.format(format).to_string()
}

fn clock(ms: i64) -> String {
    use chrono::{DateTime, Local};
    DateTime::from_timestamp_millis(ms)
        .map(|at| at.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::{Screen, Tab};
    use super::*;
    use crate::api::{ChannelKind, User};
    use crate::config::Config;

    fn msg(id: i64, author: &str, minute: i64, content: &str) -> Message {
        Message {
            id,
            author_id: Some(author.len() as i64),
            author_name: author.into(),
            content: content.into(),
            created_at: 1_700_000_000_000 + minute * 60_000,
            attachment: None,
        }
    }

    pub(super) fn app() -> super::super::App {
        let mut a = super::super::App {
            config: Config::default(),
            screen: Screen::Connecting,
        };
        let user = User {
            id: 3,
            username: "Opedro".into(),
            is_admin: false,
            avatar_url: None,
        };
        let _ = a.enter_main("https://a.b".into(), None, user, true);
        let m = main(&mut a);
        m.channels = vec![
            Channel {
                id: 1,
                name: "geral".into(),
                slug: "geral".into(),
                kind: ChannelKind::Text,
            },
            Channel {
                id: 2,
                name: "Call".into(),
                slug: "call".into(),
                kind: ChannelKind::Voice,
            },
        ];
        a
    }

    pub(super) fn main(a: &mut super::super::App) -> &mut Main {
        match &mut a.screen {
            Screen::Main(m) => m,
            _ => panic!("fora da tela principal"),
        }
    }

    fn chat(a: &mut super::super::App) -> &mut Chat {
        main(a).chat.as_mut().expect("chat aberto")
    }

    #[test]
    fn canal_de_texto_abre_e_canal_de_voz_fecha() {
        let mut a = app();
        let _ = a.update(App::Chat(2, Msg::Open));
        assert!(main(&mut a).chat.is_none(), "canal de voz não abre chat");
        let _ = a.update(App::Chat(1, Msg::Open));
        assert_eq!(chat(&mut a).channel, 1);
        let page = Page {
            messages: vec![msg(1, "ana", 0, "oi"), msg(2, "ana", 1, "")],
            has_more: false,
        };
        let _ = a.update(App::Chat(1, Msg::Loaded(Ok(Window::first(page)))));
        // Só a que tem texto ganha editor.
        assert_eq!(chat(&mut a).texts.len(), 1);
        // Resposta de outro canal não entra.
        let _ = a.update(App::Chat(9, Msg::Sent(Ok(msg(3, "ana", 2, "x")))));
        assert_eq!(chat(&mut a).timeline.messages.len(), 2);
        // Configurações abrem por cima e não fecham o chat.
        let _ = a.update(App::OpenSettings(Tab::About));
        let _ = a.update(App::CloseSettings);
        assert!(main(&mut a).chat.is_some());
        let _ = a.update(App::JoinChannel("call".into()));
        assert!(main(&mut a).chat.is_none());
    }

    #[test]
    fn enter_envia_e_shift_enter_quebra_linha() {
        let mut a = app();
        let _ = a.update(App::Chat(1, Msg::Open));
        let c = chat(&mut a);
        for ch in "oi".chars() {
            c.edit(Action::Edit(text_editor::Edit::Insert(ch)));
        }
        let focused = text_editor::Status::Focused { is_hovered: false };
        let key = |shift| KeyPress {
            key: Key::Named(Named::Enter),
            modified_key: Key::Named(Named::Enter),
            physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::Enter),
            modifiers: if shift {
                iced::keyboard::Modifiers::SHIFT
            } else {
                iced::keyboard::Modifiers::empty()
            },
            text: None,
            status: focused,
        };
        assert!(matches!(
            enter_sends(key(false), 1),
            Some(Binding::Custom(App::Chat(1, Msg::Send)))
        ));
        assert!(matches!(enter_sends(key(true), 1), Some(Binding::Enter)));

        let _ = c.update(Msg::Send, &Client::new("https://a.b", None));
        assert!(c.sending);
        // Enquanto envia, digitar não muda o campo; a resposta limpa.
        c.edit(Action::Edit(text_editor::Edit::Insert('!')));
        assert_eq!(c.draft.text(), "oi");
        let _ = c.update(
            Msg::Sent(Ok(msg(5, "Opedro", 0, "oi"))),
            &Client::new("https://a.b", None),
        );
        assert!(!c.sending);
        assert_eq!(c.draft.text(), "");
        assert_eq!(c.timeline.messages.len(), 1);
    }

    #[test]
    fn tempo_real_entra_sai_e_avisa_a_queda() {
        use crate::api::ChatEvent;
        let mut a = app();
        let _ = a.update(App::Chat(1, Msg::Open));
        let live = |l| App::Chat(1, Msg::Live(l));
        let _ = a.update(live(Live::Connected));
        assert!(chat(&mut a).live);
        let _ = a.update(live(Live::Event(ChatEvent::Created {
            message: msg(7, "ana", 0, "nova"),
        })));
        let _ = a.update(live(Live::Event(ChatEvent::Created {
            message: msg(7, "ana", 0, "nova"),
        })));
        assert_eq!(chat(&mut a).timeline.messages.len(), 1);
        assert!(chat(&mut a).texts.contains_key(&7));
        let _ = a.update(live(Live::Event(ChatEvent::Deleted { message_id: 7 })));
        assert!(chat(&mut a).timeline.messages.is_empty());
        assert!(chat(&mut a).texts.is_empty());
        let _ = a.update(live(Live::Lost));
        assert!(chat(&mut a).lost && !chat(&mut a).live);
        let _ = a.update(live(Live::Connected));
        assert!(!chat(&mut a).lost);
    }

    #[test]
    fn campo_corta_no_limite_e_texto_da_mensagem_nao_edita() {
        let mut a = app();
        let _ = a.update(App::Chat(1, Msg::Open));
        let c = chat(&mut a);
        c.edit(Action::Edit(text_editor::Edit::Paste(std::sync::Arc::new(
            "a".repeat(5000),
        ))));
        assert_eq!(c.draft.text().len(), logic::MAX_TEXTO);
        let page = Page {
            messages: vec![msg(1, "ana", 0, "texto fixo")],
            has_more: false,
        };
        let _ = c.update(
            Msg::Loaded(Ok(Window::first(page))),
            &Client::new("https://a.b", None),
        );
        let cliente = Client::new("https://a.b", None);
        let _ = c.update(Msg::Select(1, Action::SelectAll), &cliente);
        let _ = c.update(
            Msg::Select(1, Action::Edit(text_editor::Edit::Delete)),
            &cliente,
        );
        assert_eq!(c.texts[&1].text(), "texto fixo");
        assert_eq!(c.texts[&1].selection().as_deref(), Some("texto fixo"));
    }

    /// Mensagens de exemplo pros prints: blocos do mesmo autor, outro autor, texto longo.
    pub(super) fn conversa() -> Vec<Message> {
        vec![
            msg(1, "Ana", 0, "alguém vai jogar hoje?"),
            msg(2, "Ana", 1, "tô pensando em umas 21h"),
            msg(3, "Bruno", 2, "bora, mas só depois da janta"),
            msg(
                4,
                "Bruno",
                3,
                "e alguém lembra de trazer a lista dos mods? da última vez demorou uns 40 \
                 minutos pra todo mundo conseguir entrar no servidor e metade desistiu antes",
            ),
            msg(5, "Opedro", 20, "eu levo\ncom a versão nova do modpack"),
            msg(6, "Opedro", 21, "👍"),
        ]
    }

    /// Gera `snapshots/chat-*.png` sem abrir janela: `cargo test -- --ignored prints_do_chat`.
    /// Rodar sozinho: o `prints_das_telas` apaga a pasta ao começar.
    #[test]
    #[ignore]
    fn prints_do_chat() {
        use iced::{Settings, Size};
        use iced_test::simulator::Simulator;

        fn print(a: &super::super::App, nome: &str) {
            let settings = Settings {
                fonts: theme::FONTS.map(Into::into).to_vec(),
                default_font: theme::BODY,
                ..Settings::default()
            };
            let mut ui = Simulator::with_size(settings, Size::new(1200.0, 760.0), a.view());
            let print = ui.snapshot(&a.theme()).unwrap();
            assert!(
                print
                    .matches_image(format!("snapshots/chat-{nome}.png"))
                    .unwrap()
            );
        }
        let cliente = Client::new("https://a.b", None);

        let mut a = app();
        let _ = a.update(App::Chat(1, Msg::Open));
        print(&a, "carregando");
        let vazio = Page {
            messages: Vec::new(),
            has_more: false,
        };
        let _ = a.update(App::Chat(1, Msg::Loaded(Ok(Window::first(vazio)))));
        print(&a, "vazio");

        let page = Page {
            messages: conversa(),
            has_more: true,
        };
        let _ = a.update(App::Chat(1, Msg::Loaded(Ok(Window::first(page)))));
        let _ = a.update(App::Chat(1, Msg::Hover(2, true)));
        print(&a, "mensagens");

        let c = chat(&mut a);
        for ch in "mensagem nova".chars() {
            c.edit(Action::Edit(text_editor::Edit::Insert(ch)));
        }
        let _ = c.update(Msg::Send, &cliente);
        c.loading_more = true;
        print(&a, "enviando");
        let c = chat(&mut a);
        let _ = c.update(Msg::Sent(Err(Error::Offline)), &cliente);
        c.lost = true;
        print(&a, "erro");
    }
}
