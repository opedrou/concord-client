//! Conexão com a sala do LiveKit: entrar, publicar o mic, ouvir os outros e reconectar.
#![allow(dead_code, unused_variables)]
use tokio::sync::mpsc::UnboundedReceiver;

use crate::api::Client;
use crate::audio::Engine;

/// Máximo de tentativas de reconexão, como na web.
pub const MAX_ATTEMPTS: u32 = 10;

/// Quem está na sala agora. O local vem primeiro.
#[derive(Clone, Debug, PartialEq)]
pub struct Live {
    pub identity: String,
    pub name: String,
    pub is_local: bool,
    pub muted: bool,
    pub camera: bool,
    pub screen_share: bool,
    pub speaking: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum End {
    /// A pessoa saiu.
    Left,
    /// Esta conta entrou de outro lugar com a mesma instalação.
    Duplicate,
    /// Removida da sala.
    Removed,
    /// Sala fechada no servidor.
    RoomClosed,
    /// Sessão vencida ao pedir token.
    Unauthorized,
    /// Esgotou as tentativas, ou a sala recusou entrar.
    GaveUp,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Connected,
    /// Caiu e está tentando voltar: tentativa `attempt` de [`MAX_ATTEMPTS`].
    Reconnecting {
        attempt: u32,
    },
    /// Lista inteira, a cada mudança.
    Participants(Vec<Live>),
    /// Último evento da call.
    Ended(End),
}

#[derive(Clone)]
pub struct Call {}

impl Call {
    /// Conecta em segundo plano. `install_id` é o sufixo fixo da identity.
    pub fn join(
        client: Client,
        slug: String,
        install_id: String,
        engine: Engine,
        mic_muted: bool,
    ) -> (Call, UnboundedReceiver<Event>) {
        todo!()
    }

    pub fn set_mic_muted(&self, muted: bool) {
        todo!()
    }

    /// Nomes com voz em zero: vai pro atributo `concord.muted`.
    pub fn set_muted_people(&self, names: Vec<String>) {
        todo!()
    }

    /// Sai de forma limpa, com `room.close()`. Termina em `Ended(End::Left)`.
    pub fn leave(&self) {
        todo!()
    }
}
