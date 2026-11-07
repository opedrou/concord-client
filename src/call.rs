//! Conexão com a sala do LiveKit: entrar, publicar o mic, ouvir os outros e reconectar.
// Some quando a interface passar a chamar `Call::join`.
#![allow(dead_code)]
use std::{collections::HashMap, fmt, future::Future, time::Duration};

use futures_util::StreamExt;
use livekit::{
    DisconnectReason, Room, RoomEvent, RoomOptions,
    options::TrackPublishOptions,
    publication::LocalTrackPublication,
    track::{LocalAudioTrack, LocalTrack, RemoteTrack, TrackSource},
    webrtc::{
        audio_source::native::NativeAudioSource,
        audio_stream::native::NativeAudioStream,
        prelude::{AudioFrame, AudioSourceOptions, RtcAudioSource},
    },
};
use tokio::{
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        watch,
    },
    task::JoinHandle,
    time::{Instant, sleep, sleep_until, timeout},
};

use crate::api::{self, Client};
use crate::audio::{Engine, MicFrames, RATE};

/// Máximo de tentativas de reconexão, como na web.
pub const MAX_ATTEMPTS: u32 = 10;

/// Atributo de participante com os nomes que esta pessoa deixou em volume zero.
const MUTED_ATTRIBUTE: &str = "concord.muted";
const MUTED_DEBOUNCE: Duration = Duration::from_millis(300);
/// Sem isso um `close()` travado seguraria o `Ended` pra sempre.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

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

enum Cmd {
    Mute(bool),
    MutedPeople(Vec<String>),
    Leave,
}

#[derive(Clone)]
pub struct Call {
    cmd: UnboundedSender<Cmd>,
    engine: Engine,
}

impl fmt::Debug for Call {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Call").finish_non_exhaustive()
    }
}

impl Call {
    /// Conecta em segundo plano. `install_id` é o sufixo fixo da identity. `mic` são
    /// os blocos que o [`Engine::start`] devolveu; a call os consome até acabar.
    pub fn join(
        client: Client,
        slug: String,
        install_id: String,
        engine: Engine,
        mic: MicFrames,
        mic_muted: bool,
    ) -> (Call, UnboundedReceiver<Event>) {
        // Fecha o mic antes de qualquer coisa: entrar nunca abre o mic sozinho.
        if mic_muted {
            engine.set_mic_muted(true);
        }
        let (tx, events) = mpsc::unbounded_channel();
        let (cmd, rx) = mpsc::unbounded_channel();
        let task = Task {
            client,
            slug,
            install_id,
            tx,
        };
        let ctl = Ctl {
            rx,
            engine: engine.clone(),
            muted: mic_muted,
            people: Vec::new(),
        };
        tokio::spawn(task.run(ctl, mic));
        (Call { cmd, engine }, events)
    }

    pub fn set_mic_muted(&self, muted: bool) {
        // Mutar vale na hora, sem esperar a tarefa; desmutar passa pela tarefa, que
        // abre a faixa antes do motor.
        if muted {
            self.engine.set_mic_muted(true);
        }
        let _ = self.cmd.send(Cmd::Mute(muted));
    }

    /// Nomes com voz em zero: vai pro atributo `concord.muted`.
    pub fn set_muted_people(&self, names: Vec<String>) {
        let _ = self.cmd.send(Cmd::MutedPeople(names));
    }

    /// Sai de forma limpa, com `room.close()`. Termina em `Ended(End::Left)`.
    pub fn leave(&self) {
        let _ = self.cmd.send(Cmd::Leave);
    }
}

// --- Regras puras ------------------------------------------------------------

/// Motivo de desconexão definitivo vira o fim da call. Desconhecido é transitório,
/// como na web (`isDefinitiveDisconnect`).
fn definitive(reason: DisconnectReason) -> Option<End> {
    match reason {
        DisconnectReason::ClientInitiated => Some(End::Left),
        DisconnectReason::DuplicateIdentity => Some(End::Duplicate),
        DisconnectReason::ParticipantRemoved => Some(End::Removed),
        DisconnectReason::RoomDeleted | DisconnectReason::RoomClosed => Some(End::RoomClosed),
        _ => None,
    }
}

/// Espera antes da tentativa `attempt` (a primeira é 1): `min(2000·2^n, 30000)` ms.
fn backoff(attempt: u32) -> Duration {
    let n = attempt.saturating_sub(1).min(10);
    Duration::from_millis((2000u64 << n).min(30_000))
}

/// Nomes ordenados e sem repetição, separados por vírgula: igual a `encodeMuted` da web.
fn encode_muted(names: &[String]) -> String {
    let mut v: Vec<&str> = names.iter().map(String::as_str).collect();
    v.sort_unstable();
    v.dedup();
    v.join(",")
}

/// Monta uma pessoa a partir das publicações. Sem mic publicado conta como mudo, a
/// mesma regra do servidor em `/api/channels/presence`.
fn live(
    identity: String,
    name: String,
    is_local: bool,
    pubs: impl IntoIterator<Item = (TrackSource, bool)>,
    speaking: bool,
) -> Live {
    let (mut mic_open, mut camera, mut screen_share) = (false, false, false);
    for (source, muted) in pubs {
        match source {
            TrackSource::Microphone => mic_open |= !muted,
            TrackSource::Camera => camera |= !muted,
            TrackSource::Screenshare => screen_share = true,
            _ => {}
        }
    }
    Live {
        name: if name.is_empty() {
            identity.clone()
        } else {
            name
        },
        identity,
        is_local,
        muted: !mic_open,
        camera,
        screen_share,
        speaking,
    }
}

/// Local primeiro; os outros em ordem estável, pra lista não pular de lugar.
fn roster(local: Live, mut remotes: Vec<Live>) -> Vec<Live> {
    remotes.sort_by_cached_key(|p| (p.name.to_lowercase(), p.identity.clone()));
    let mut all = vec![local];
    all.extend(remotes);
    all
}

// --- Tarefa da call ----------------------------------------------------------

struct Task {
    client: Client,
    slug: String,
    install_id: String,
    tx: UnboundedSender<Event>,
}

/// Estado que atravessa as reconexões.
struct Ctl {
    rx: UnboundedReceiver<Cmd>,
    engine: Engine,
    /// Último estado pedido do mic.
    muted: bool,
    people: Vec<String>,
}

enum Fail {
    Unauthorized,
    Leave,
    Retry,
}

enum Outcome {
    End(End),
    /// Caiu por motivo transitório.
    Dropped,
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Voice {
    identity: String,
    _task: AbortOnDrop,
}

/// Uma conexão com a sala. Soltar a sessão solta as faixas e as vozes.
struct Session {
    room: Room,
    events: UnboundedReceiver<RoomEvent>,
    mic: LocalTrackPublication,
    voices: HashMap<String, Voice>,
    last: Vec<Live>,
    /// Último `concord.muted` enviado nesta sala.
    sent_muted: String,
}

impl Ctl {
    /// Roda `fut` atendendo os comandos; `None` se pediram pra sair antes de acabar.
    async fn or_leave<T>(&mut self, fut: impl Future<Output = T>) -> Option<T> {
        tokio::pin!(fut);
        loop {
            tokio::select! {
                r = &mut fut => return Some(r),
                cmd = self.rx.recv() => match cmd {
                    None | Some(Cmd::Leave) => return None,
                    Some(Cmd::Mute(m)) => {
                        self.muted = m;
                        self.engine.set_mic_muted(m);
                    }
                    Some(Cmd::MutedPeople(p)) => self.people = p,
                }
            }
        }
    }
}

/// Fechar o motor antes da faixa, abrir a faixa antes do motor: nenhum bloco sai
/// por uma faixa que ainda não está no estado certo.
fn apply_mute(engine: &Engine, mic: Option<&LocalTrackPublication>, muted: bool) {
    if muted {
        engine.set_mic_muted(true);
        if let Some(m) = mic {
            m.mute();
        }
    } else {
        if let Some(m) = mic {
            m.unmute();
        }
        engine.set_mic_muted(false);
    }
}

/// Dono do `MicFrames`: manda cada bloco pra faixa da sala atual, ou descarta se
/// não há sala.
async fn forward_mic(mut mic: MicFrames, source: watch::Receiver<Option<NativeAudioSource>>) {
    while let Some(block) = mic.recv().await {
        let Some(src) = source.borrow().clone() else {
            continue;
        };
        let frame = AudioFrame {
            samples_per_channel: block.len() as u32,
            data: block.into(),
            sample_rate: RATE,
            num_channels: 1,
        };
        let _ = src.capture_frame(&frame).await;
    }
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(d) => sleep_until(d).await,
        None => std::future::pending().await,
    }
}

impl Task {
    fn emit(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    async fn run(self, mut ctl: Ctl, mic: MicFrames) {
        let (source, source_rx) = watch::channel(None);
        let _forward = AbortOnDrop(tokio::spawn(forward_mic(mic, source_rx)));
        let end = self.lifecycle(&mut ctl, &source).await;
        // Faixa e vozes já caíram aqui: `Ended` é sempre o último evento.
        self.emit(Event::Ended(end));
    }

    async fn lifecycle(
        &self,
        ctl: &mut Ctl,
        source: &watch::Sender<Option<NativeAudioSource>>,
    ) -> End {
        let mut attempt = 0;
        loop {
            match self.connect(ctl, source).await {
                Ok(mut session) => {
                    attempt = 0;
                    self.emit(Event::Connected);
                    let outcome = self.serve(&mut session, ctl).await;
                    source.send_replace(None);
                    drop(session);
                    if let Outcome::End(end) = outcome {
                        return end;
                    }
                }
                Err(Fail::Unauthorized) => return End::Unauthorized,
                Err(Fail::Leave) => return End::Left,
                Err(Fail::Retry) => {}
            }
            attempt += 1;
            if attempt > MAX_ATTEMPTS {
                return End::GaveUp;
            }
            self.emit(Event::Reconnecting { attempt });
            if ctl.or_leave(sleep(backoff(attempt))).await.is_none() {
                return End::Left;
            }
        }
    }

    /// Token novo a cada tentativa: o de 5 min já pode ter vencido.
    async fn connect(
        &self,
        ctl: &mut Ctl,
        source: &watch::Sender<Option<NativeAudioSource>>,
    ) -> Result<Session, Fail> {
        let details = ctl
            .or_leave(self.client.connection_details(&self.slug, &self.install_id))
            .await
            .ok_or(Fail::Leave)?
            .map_err(|e| match e {
                api::Error::Unauthorized => Fail::Unauthorized,
                _ => Fail::Retry,
            })?;
        let mut options = RoomOptions::default();
        options.auto_subscribe = false;
        let (room, events) = ctl
            .or_leave(Room::connect(
                &details.server_url,
                &details.participant_token,
                options,
            ))
            .await
            .ok_or(Fail::Leave)?
            .map_err(|_| Fail::Retry)?;

        let src = NativeAudioSource::new(AudioSourceOptions::default(), RATE, 1, 200);
        let track =
            LocalAudioTrack::create_audio_track("microphone", RtcAudioSource::Native(src.clone()));
        let options = TrackPublishOptions {
            source: TrackSource::Microphone,
            ..Default::default()
        };
        let mic = match room
            .local_participant()
            .publish_track(LocalTrack::Audio(track), options)
            .await
        {
            Ok(p) => p,
            Err(_) => {
                let _ = room.close().await;
                return Err(Fail::Retry);
            }
        };
        // Publica mudo antes de a faixa receber o primeiro bloco.
        apply_mute(&ctl.engine, Some(&mic), ctl.muted);
        source.send_replace(Some(src));

        Ok(Session {
            room,
            events,
            mic,
            voices: HashMap::new(),
            last: Vec::new(),
            sent_muted: String::new(),
        })
    }

    async fn serve(&self, s: &mut Session, ctl: &mut Ctl) -> Outcome {
        s.subscribe_all();
        s.send_muted(&ctl.people);
        self.refresh(s);
        let mut debounce: Option<Instant> = None;
        loop {
            tokio::select! {
                event = s.events.recv() => {
                    // Canal fechado sem `Disconnected`: queda sem motivo, transitória.
                    let Some(event) = event else { return Outcome::Dropped };
                    if let Some(outcome) = self.on_event(s, ctl, event) {
                        return outcome;
                    }
                }
                cmd = ctl.rx.recv() => match cmd {
                    Some(Cmd::Mute(m)) => {
                        ctl.muted = m;
                        apply_mute(&ctl.engine, Some(&s.mic), m);
                        self.refresh(s);
                    }
                    Some(Cmd::MutedPeople(p)) => {
                        ctl.people = p;
                        debounce = Some(Instant::now() + MUTED_DEBOUNCE);
                    }
                    None | Some(Cmd::Leave) => return Outcome::End(s.close().await),
                },
                _ = wait_until(debounce) => {
                    debounce = None;
                    s.send_muted(&ctl.people);
                }
                // A interface soltou o receptor: sai limpo, sem ninguém pra avisar.
                _ = self.tx.closed() => return Outcome::End(s.close().await),
            }
        }
    }

    fn on_event(&self, s: &mut Session, ctl: &Ctl, event: RoomEvent) -> Option<Outcome> {
        match &event {
            RoomEvent::Disconnected { reason } => {
                return Some(match definitive(*reason) {
                    Some(end) => Outcome::End(end),
                    None => Outcome::Dropped,
                });
            }
            RoomEvent::TrackPublished { publication, .. }
                if publication.source() == TrackSource::Microphone =>
            {
                publication.set_subscribed(true);
            }
            RoomEvent::TrackSubscribed {
                track: RemoteTrack::Audio(audio),
                publication,
                participant,
            } if publication.source() == TrackSource::Microphone => {
                let sid = publication.sid().to_string();
                // Solta a voz antiga antes: a mesma chave não pode sair do mixer
                // depois de a nova entrar.
                s.voices.remove(&sid);
                let identity = participant.identity().to_string();
                let name = participant.name();
                let name = if name.is_empty() {
                    identity.clone()
                } else {
                    name
                };
                let mut sink = ctl.engine.add_voice(sid.clone(), name);
                let mut stream = NativeAudioStream::new(audio.rtc_track(), RATE as i32, 1);
                let task = tokio::spawn(async move {
                    while let Some(frame) = stream.next().await {
                        sink.push(&frame.data);
                    }
                });
                s.voices.insert(
                    sid,
                    Voice {
                        identity,
                        _task: AbortOnDrop(task),
                    },
                );
            }
            RoomEvent::TrackUnsubscribed { publication, .. } => {
                s.voices.remove(&publication.sid().to_string());
            }
            RoomEvent::ParticipantDisconnected(p) => {
                let identity = p.identity().to_string();
                s.voices.retain(|_, v| v.identity != identity);
            }
            RoomEvent::Reconnected => {
                // A retomada pode ter perdido assinaturas e atributos.
                s.subscribe_all();
                s.sent_muted.clear();
                s.send_muted(&ctl.people);
            }
            RoomEvent::LocalTrackRepublished { publication, .. }
                if publication.source() == TrackSource::Microphone =>
            {
                s.mic = publication.clone();
                apply_mute(&ctl.engine, Some(&s.mic), ctl.muted);
            }
            _ => {}
        }
        if matches!(
            event,
            RoomEvent::ParticipantConnected(_)
                | RoomEvent::ParticipantDisconnected(_)
                | RoomEvent::ParticipantNameChanged { .. }
                | RoomEvent::TrackPublished { .. }
                | RoomEvent::TrackUnpublished { .. }
                | RoomEvent::LocalTrackPublished { .. }
                | RoomEvent::LocalTrackUnpublished { .. }
                | RoomEvent::LocalTrackRepublished { .. }
                | RoomEvent::TrackMuted { .. }
                | RoomEvent::TrackUnmuted { .. }
                | RoomEvent::ActiveSpeakersChanged { .. }
                | RoomEvent::Reconnected
        ) {
            self.refresh(s);
        }
        None
    }

    fn refresh(&self, s: &mut Session) {
        let list = s.participants();
        if list != s.last {
            s.last.clone_from(&list);
            self.emit(Event::Participants(list));
        }
    }
}

impl Session {
    /// Só a voz: tela, áudio de tela e `concord-app-audio` ficam sem assinar.
    fn subscribe_all(&self) {
        for p in self.room.remote_participants().values() {
            for publication in p.track_publications().values() {
                if publication.source() == TrackSource::Microphone {
                    publication.set_subscribed(true);
                }
            }
        }
    }

    fn participants(&self) -> Vec<Live> {
        let me = self.room.local_participant();
        let local = live(
            me.identity().to_string(),
            me.name(),
            true,
            me.track_publications()
                .values()
                .map(|p| (p.source(), p.is_muted())),
            me.is_speaking(),
        );
        let remotes = self
            .room
            .remote_participants()
            .values()
            .map(|p| {
                live(
                    p.identity().to_string(),
                    p.name(),
                    false,
                    p.track_publications()
                        .values()
                        .map(|t| (t.source(), t.is_muted())),
                    p.is_speaking(),
                )
            })
            .collect();
        roster(local, remotes)
    }

    /// Anuncia `concord.muted` só se mudou. Em segundo plano, pra não segurar os eventos.
    fn send_muted(&mut self, people: &[String]) {
        let value = encode_muted(people);
        if value == self.sent_muted {
            return;
        }
        self.sent_muted.clone_from(&value);
        let me = self.room.local_participant();
        tokio::spawn(async move {
            let _ = me
                .set_attributes(HashMap::from([(MUTED_ATTRIBUTE.to_owned(), value)]))
                .await;
        });
    }

    /// Saída limpa: sem isso o nome fica preso na lista do servidor até o timeout.
    async fn close(&self) -> End {
        let _ = timeout(CLOSE_TIMEOUT, self.room.close()).await;
        End::Left
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motivos_definitivos() {
        use DisconnectReason::*;
        assert_eq!(definitive(ClientInitiated), Some(End::Left));
        assert_eq!(definitive(DuplicateIdentity), Some(End::Duplicate));
        assert_eq!(definitive(ParticipantRemoved), Some(End::Removed));
        assert_eq!(definitive(RoomDeleted), Some(End::RoomClosed));
        assert_eq!(definitive(RoomClosed), Some(End::RoomClosed));
    }

    #[test]
    fn motivos_transitorios() {
        use DisconnectReason::*;
        for r in [
            UnknownReason,
            ServerShutdown,
            StateMismatch,
            JoinFailure,
            Migration,
            SignalClose,
            ConnectionTimeout,
            MediaFailure,
        ] {
            assert_eq!(definitive(r), None, "{r:?}");
        }
    }

    #[test]
    fn espera_dobra_ate_30s() {
        let ms: Vec<u128> = (1..=MAX_ATTEMPTS + 1)
            .map(|n| backoff(n).as_millis())
            .collect();
        assert_eq!(
            ms,
            [
                2000, 4000, 8000, 16000, 30000, 30000, 30000, 30000, 30000, 30000, 30000
            ]
        );
        // Sem estouro em contagem absurda.
        assert_eq!(backoff(u32::MAX).as_millis(), 30_000);
    }

    #[test]
    fn muted_ordenado_sem_repetir() {
        let n = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(encode_muted(&[]), "");
        assert_eq!(
            encode_muted(&n(&["zeca", "ana", "zeca", "bia"])),
            "ana,bia,zeca"
        );
    }

    fn pessoa(id: &str, name: &str, pubs: &[(TrackSource, bool)]) -> Live {
        live(id.into(), name.into(), false, pubs.iter().copied(), false)
    }

    #[test]
    fn sem_mic_conta_como_mudo() {
        assert!(pessoa("a__1", "a", &[]).muted);
        assert!(pessoa("a__1", "a", &[(TrackSource::Microphone, true)]).muted);
        assert!(!pessoa("a__1", "a", &[(TrackSource::Microphone, false)]).muted);
    }

    #[test]
    fn camera_tela_e_nome() {
        let p = pessoa(
            "a__1",
            "",
            &[
                (TrackSource::Microphone, false),
                (TrackSource::Camera, true),
                (TrackSource::Screenshare, true),
                (TrackSource::ScreenshareAudio, false),
            ],
        );
        assert_eq!(p.name, "a__1");
        assert!(!p.camera, "câmera muda não conta");
        assert!(p.screen_share, "tela conta mesmo pausada");
        let c = pessoa("a__1", "a", &[(TrackSource::Camera, false)]);
        assert!(c.camera);
    }

    #[test]
    fn local_primeiro_e_ordem_estavel() {
        let local = live("eu__1".into(), "Zé".into(), true, [], false);
        let all = roster(
            local.clone(),
            vec![
                pessoa("b__1", "bia", &[]),
                pessoa("a__2", "Ana", &[]),
                pessoa("a__1", "Ana", &[]),
            ],
        );
        let ids: Vec<_> = all.iter().map(|p| p.identity.as_str()).collect();
        assert_eq!(ids, ["eu__1", "a__1", "a__2", "b__1"]);
        assert!(all[0].is_local);
    }
}
