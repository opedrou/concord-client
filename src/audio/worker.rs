//! Thread de áudio: dona do APM, das filas e dos streams do cpal, que não são `Send`.
use std::{
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use tokio::sync::mpsc::Sender;

use super::{
    AudioPrefs, FRAME, Meter, MicGain, NoiseLevel, Sensitivity,
    device::{self, Opened, Seen},
    mic::MicChain,
    mixer::{Mixer, VoiceQueue},
};

const TICK: Duration = Duration::from_millis(2);
const RETRY: Duration = Duration::from_secs(2);
/// De quanto em quanto o servidor é consultado pra ver se o stream está no lugar certo.
const LOOK: Duration = Duration::from_secs(1);
/// Teto da espera pelo som de saída com o motor solto: os sons têm menos de 3 s. Saída
/// travada não pode segurar a thread pra sempre.
const LINGER: Duration = Duration::from_secs(3);
/// De quanto em quanto as filas que descartaram aparecem no log.
const REPORT: Duration = Duration::from_secs(5);

pub enum Command {
    Mute(bool),
    Deafen(bool),
    Master(u8),
    Person(String, u8),
    Noise(NoiseLevel),
    Sensitivity(Sensitivity),
    Gain(MicGain),
    Input(Option<String>),
    Output(Option<String>),
    Effect(&'static [u8]),
    Voice {
        key: String,
        name: String,
        queue: Arc<VoiceQueue>,
    },
}

/// Um lado do motor: o que está aberto, o que foi pedido e quando tentar de novo.
struct Side<T> {
    stream: Option<T>,
    wanted: Option<String>,
    next_try: Instant,
}

impl<T> Side<T> {
    fn new(wanted: Option<String>) -> Self {
        Side {
            stream: None,
            wanted,
            next_try: Instant::now(),
        }
    }

    fn switch(&mut self, wanted: Option<String>) {
        *self = Side::new(wanted);
    }

    /// Perdido reabre no salvo, que o `open` troca pelo padrão enquanto ele não existir.
    fn keep_open(
        &mut self,
        now: Instant,
        is_lost: impl FnOnce(&T) -> bool,
        open: impl FnOnce(Option<&str>) -> Result<T, String>,
    ) {
        if self.stream.as_ref().is_some_and(is_lost) {
            // `next_try` fica como está: stream que nasce morto não vira reabertura em laço.
            self.stream = None;
        }
        if self.stream.is_none() && now >= self.next_try {
            self.next_try = now + RETRY;
            self.stream = open(self.wanted.as_deref()).ok();
        }
    }

    /// Fecha o stream se ele não está onde devia: padrão novo, ou o salvo que ligou ou
    /// desligou. O servidor empurra o stream pro padrão quando o fone cai, sem erro, e
    /// não traz de volta quando ele volta. Devolve o stream fechado; o
    /// [`Side::keep_open`] reabre na hora.
    fn follow(
        &mut self,
        now: Instant,
        seen: &Seen,
        id: impl FnOnce(&T) -> Option<&str>,
    ) -> Option<T> {
        let target = seen.target(self.wanted.as_deref())?;
        if self.stream.as_ref().and_then(id)? == target {
            return None;
        }
        self.next_try = now;
        self.stream.take()
    }
}

/// Consulta o servidor fora da thread de áudio, que só tem 30 ms de folga e não pode
/// travar junto com ele. Acaba quando a thread de áudio acaba.
fn watch(seen: mpsc::Sender<(Seen, Seen)>) {
    let mut host = None;
    loop {
        thread::sleep(LOOK);
        if host.is_none() {
            host = device::host().ok();
        }
        let Some(now) = host.as_ref().and_then(device::look) else {
            // Conexão caída: a próxima volta abre outra.
            host = None;
            continue;
        };
        if seen.send(now).is_err() {
            return;
        }
    }
}

/// Avisa no log quando a thread acaba, inclusive em pânico.
struct Stopped;

impl Drop for Stopped {
    fn drop(&mut self) {
        let how = if thread::panicking() {
            " em pânico"
        } else {
            ""
        };
        log!("áudio: motor parou{how}");
    }
}

/// A mesma falha repetida a cada [`RETRY`] entra no log uma vez só.
fn opened<R>(
    what: &str,
    last_error: &mut Option<String>,
    result: Result<Opened<R>, String>,
) -> Result<Opened<R>, String> {
    match &result {
        Ok(s) => {
            *last_error = None;
            log!(
                "áudio: {what} aberta em {} ({})",
                s.name,
                s.id.as_deref().unwrap_or("?")
            );
        }
        Err(e) if last_error.as_ref() != Some(e) => {
            log!("áudio: {what} não abriu: {e}");
            *last_error = Some(e.clone());
        }
        Err(_) => {}
    }
    result
}

fn lost<R>(what: &str, stream: &Opened<R>) -> bool {
    let lost = stream.is_lost();
    if lost {
        log!("áudio: {what} perdida em {}, reabrindo", stream.name);
    }
    lost
}

pub fn run(
    prefs: AudioPrefs,
    meter: Meter,
    commands: mpsc::Receiver<Command>,
    frames: Sender<Vec<i16>>,
    ready: mpsc::Sender<Result<(), String>>,
) {
    let mut mixer = Mixer::new(prefs.master, prefs.volumes);
    let mut chain = MicChain::new(prefs.noise, prefs.sensitivity, prefs.mic_gain, meter);
    let mut input = Side::new(prefs.input);
    let mut output = Side::new(prefs.output);
    let mut mix = [0f32; FRAME];
    let mut pcm = [0i16; FRAME];
    // Motor solto com som de saída tocando: só a saída segue, até o som acabar ou o prazo.
    let mut closing: Option<Instant> = None;
    let (seen_tx, seen) = mpsc::channel();
    // Sem a thread, o motor só não acompanha a troca de dispositivo do sistema.
    let _ = thread::Builder::new()
        .name("concord-disp".into())
        .spawn(move || watch(seen_tx));
    let mut report = Instant::now() + REPORT;
    log!("áudio: motor começou");
    let _stopped = Stopped;

    let mut errors: [Option<String>; 2] = Default::default();
    let mut keep_open = |output: &mut Side<_>, input: &mut Side<_>| {
        let now = Instant::now();
        output.keep_open(
            now,
            |s| lost("saída", s),
            |n| opened("saída", &mut errors[0], device::open_output(n)),
        );
        input.keep_open(
            now,
            |s| lost("entrada", s),
            |n| opened("entrada", &mut errors[1], device::open_input(n)),
        );
    };
    keep_open(&mut output, &mut input);
    // Um lado só já dá call: quem está sem mic ainda ouve, e o mic entra quando aparecer.
    if output.stream.is_none() && input.stream.is_none() {
        let _ = ready.send(Err("nenhum dispositivo de áudio abriu".into()));
        return;
    }
    let _ = ready.send(Ok(()));

    loop {
        let mut next = match commands.recv_timeout(TICK) {
            Ok(command) => Some(command),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let deadline = *closing.get_or_insert_with(|| Instant::now() + LINGER);
                let stuck = output.stream.as_ref().is_none_or(Opened::is_lost)
                    || Instant::now() >= deadline;
                if !mixer.playing() || stuck {
                    return;
                }
                input.stream = None;
                thread::sleep(TICK);
                None
            }
        };
        while let Some(command) = next {
            match command {
                Command::Mute(muted) => {
                    if chain.muted != muted {
                        log!("áudio: mic {}", if muted { "mudo" } else { "aberto" });
                    }
                    chain.muted = muted;
                }
                Command::Deafen(deafened) => {
                    if mixer.deafened != deafened {
                        log!("áudio: {}", if deafened { "surdo" } else { "ouvindo" });
                    }
                    mixer.deafened = deafened;
                }
                Command::Master(slider) => mixer.master = slider,
                Command::Person(name, slider) => mixer.set_person(name, slider),
                Command::Noise(level) => chain.set_noise(level),
                Command::Sensitivity(sensitivity) => chain.gate.set_sensitivity(sensitivity),
                Command::Gain(gain) => chain.set_gain(gain),
                Command::Input(name) => {
                    log!(
                        "áudio: entrada escolhida: {}",
                        name.as_deref().unwrap_or("padrão")
                    );
                    input.switch(name);
                }
                Command::Output(name) => {
                    log!(
                        "áudio: saída escolhida: {}",
                        name.as_deref().unwrap_or("padrão")
                    );
                    output.switch(name);
                }
                Command::Effect(pcm) => mixer.play(pcm),
                Command::Voice { key, name, queue } => mixer.add(key, name, queue),
            }
            next = commands.try_recv().ok();
        }
        if closing.is_none() {
            if let Some((outputs, inputs)) = seen.try_iter().last() {
                let now = Instant::now();
                if let Some(old) = output.follow(now, &outputs, |s| s.id.as_deref()) {
                    log!("áudio: saída fora do lugar em {}, reabrindo", old.name);
                }
                if let Some(old) = input.follow(now, &inputs, |s| s.id.as_deref()) {
                    log!("áudio: entrada fora do lugar em {}, reabrindo", old.name);
                }
            }
            keep_open(&mut output, &mut input);
        }
        if Instant::now() >= report {
            report += REPORT;
            for (name, ms) in mixer.dropped() {
                log!("áudio: fila de {name} descartou {ms} ms por estar cheia");
            }
        }

        // Reverse antes do forward: o APM precisa ver o som antes do eco dele.
        if let Some(stream) = &mut output.stream {
            while stream.ring.slots() >= FRAME {
                mixer.mix(&mut mix);
                for (p, s) in pcm.iter_mut().zip(mix) {
                    *p = (s * 32767.0) as i16;
                    let _ = stream.ring.push(s);
                }
                chain.reverse(&mut pcm);
            }
        }
        if let Some(stream) = &mut input.stream {
            while stream.ring.slots() >= FRAME {
                for p in &mut pcm {
                    *p = (stream.ring.pop().unwrap_or(0.0) * 32767.0) as i16;
                }
                if let Some(block) = chain.process(&mut pcm) {
                    // Sala parada perde bloco em vez de acumular atraso.
                    let _ = frames.try_send(block);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn stream_perdido_reabre_no_salvo() {
        let mut side = Side::new(Some("fone".to_string()));
        let now = Instant::now();
        let asked = RefCell::new(vec![]);
        let open = |name: Option<&str>| {
            asked.borrow_mut().push(name.map(String::from));
            Ok(())
        };
        side.keep_open(now, |_| false, open);
        side.keep_open(now + RETRY / 2, |_| true, open);
        assert_eq!(asked.borrow().len(), 1, "respeita o intervalo");
        side.keep_open(now + RETRY, |_| false, open);
        // Esquecer o salvo deixava a call no padrão mesmo depois de o fone voltar.
        let fone = Some("fone".to_string());
        assert_eq!(*asked.borrow(), [fone.clone(), fone]);
    }

    fn seen(default: &str, devices: &[(&str, &str)]) -> Seen {
        Seen {
            default: Some(default.into()),
            devices: devices
                .iter()
                .map(|(n, i)| (n.to_string(), i.to_string()))
                .collect(),
        }
    }

    /// Lado aberto em `id`, como o `open` teria deixado.
    fn aberto(wanted: Option<&str>, id: &str) -> Side<String> {
        let mut side = Side::new(wanted.map(String::from));
        side.stream = Some(id.to_string());
        side
    }

    fn follow(side: &mut Side<String>, now: Instant, seen: &Seen) -> Option<String> {
        side.follow(now, seen, |s| Some(s.as_str()))
    }

    #[test]
    fn fone_salvo_que_volta_reabre_nele() {
        let now = Instant::now();
        // Entrou com o fone desligado: o `open` caiu no padrão.
        let mut side = aberto(Some("Fone"), "hdmi");
        assert_eq!(
            follow(&mut side, now, &seen("hdmi", &[("HDMI", "hdmi")])),
            None
        );
        let ligou = seen("hdmi", &[("HDMI", "hdmi"), ("Fone", "bluez")]);
        assert_eq!(follow(&mut side, now, &ligou).as_deref(), Some("hdmi"));
        let asked = RefCell::new(None);
        side.keep_open(
            now,
            |_| false,
            |n| {
                *asked.borrow_mut() = n.map(String::from);
                Ok("bluez".to_string())
            },
        );
        assert_eq!(
            asked.borrow().as_deref(),
            Some("Fone"),
            "reabre já, no salvo"
        );
        assert_eq!(follow(&mut side, now, &ligou), None, "e fica");
    }

    #[test]
    fn fone_salvo_que_cai_sai_dele_e_padrao_que_muda_vai_junto() {
        let now = Instant::now();
        let mut fone = aberto(Some("Fone"), "bluez");
        let caiu = seen("hdmi", &[("HDMI", "hdmi")]);
        assert_eq!(follow(&mut fone, now, &caiu).as_deref(), Some("bluez"));
        let mut padrao = aberto(None, "hdmi");
        assert_eq!(follow(&mut padrao, now, &caiu), None);
        let mudou = seen("bluez", &[("HDMI", "hdmi"), ("Fone", "bluez")]);
        assert_eq!(follow(&mut padrao, now, &mudou).as_deref(), Some("hdmi"));
        let mut salvo = aberto(Some("Fone"), "bluez");
        assert_eq!(
            follow(&mut salvo, now, &mudou),
            None,
            "salvo ignora o padrão"
        );
    }

    #[test]
    fn sem_id_nao_reabre() {
        let now = Instant::now();
        let mut side = aberto(None, "hdmi");
        assert_eq!(side.follow(now, &seen("bluez", &[]), |_| None), None);
        assert_eq!(follow(&mut side, now, &Seen::default()), None);
        assert!(side.stream.is_some());
    }

    #[test]
    fn falha_ao_abrir_espera_o_intervalo() {
        let mut side = Side::<()>::new(None);
        let now = Instant::now();
        let tries = RefCell::new(0);
        let fail = |_: Option<&str>| {
            *tries.borrow_mut() += 1;
            Err("sumiu".to_string())
        };
        side.keep_open(now, |_| false, fail);
        side.keep_open(now + RETRY / 2, |_| false, fail);
        assert_eq!(*tries.borrow(), 1);
        side.keep_open(now + RETRY, |_| false, fail);
        assert_eq!(*tries.borrow(), 2);
    }

    #[test]
    fn stream_aberto_nao_reabre() {
        let mut side = Side::new(None);
        let now = Instant::now();
        side.keep_open(now, |_| false, |_| Ok(()));
        side.keep_open(
            now + RETRY,
            |_| false,
            |_| Err("não devia abrir".to_string()),
        );
        assert!(side.stream.is_some());
    }
}
