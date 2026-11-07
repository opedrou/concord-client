//! Thread de áudio: dona do APM, das filas e dos streams do cpal, que não são `Send`.
use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use tokio::sync::mpsc::Sender;

use super::{
    AudioPrefs, FRAME, Meter, NoiseLevel, Sensitivity,
    device::{self, Opened},
    mic::MicChain,
    mixer::{Mixer, VoiceQueue},
};

const TICK: Duration = Duration::from_millis(2);
const RETRY: Duration = Duration::from_secs(2);

pub enum Command {
    Mute(bool),
    Deafen(bool),
    Master(u8),
    Person(String, u8),
    Noise(NoiseLevel),
    // Sem uso até a interface chegar nesta branch.
    #[allow(dead_code)]
    Sensitivity(Sensitivity),
    Input(Option<String>),
    Output(Option<String>),
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

    fn keep_open(
        &mut self,
        now: Instant,
        is_lost: impl FnOnce(&T) -> bool,
        open: impl FnOnce(Option<&str>) -> Result<T, String>,
    ) {
        if self.stream.as_ref().is_some_and(is_lost) {
            // `next_try` fica como está: stream que nasce morto não vira reabertura em laço.
            self.stream = None;
            self.wanted = None;
        }
        if self.stream.is_none() && now >= self.next_try {
            self.next_try = now + RETRY;
            match open(self.wanted.as_deref()) {
                Ok(stream) => self.stream = Some(stream),
                Err(e) => eprintln!("áudio: falha ao abrir: {e}"),
            }
        }
    }
}

pub fn run(
    prefs: AudioPrefs,
    meter: Meter,
    commands: mpsc::Receiver<Command>,
    frames: Sender<Vec<i16>>,
    ready: mpsc::Sender<Result<(), String>>,
) {
    let mut mixer = Mixer::new(prefs.master, prefs.volumes);
    let mut chain = MicChain::new(prefs.noise, prefs.sensitivity, meter);
    let mut input = Side::new(prefs.input);
    let mut output = Side::new(prefs.output);
    let mut mix = [0f32; FRAME];
    let mut pcm = [0i16; FRAME];

    let keep_open = |output: &mut Side<_>, input: &mut Side<_>| {
        let now = Instant::now();
        output.keep_open(now, Opened::is_lost, device::open_output);
        input.keep_open(now, Opened::is_lost, device::open_input);
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
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        while let Some(command) = next {
            match command {
                Command::Mute(muted) => chain.muted = muted,
                Command::Deafen(deafened) => mixer.deafened = deafened,
                Command::Master(slider) => mixer.master = slider,
                Command::Person(name, slider) => mixer.set_person(name, slider),
                Command::Noise(level) => chain.set_noise(level),
                Command::Sensitivity(sensitivity) => chain.gate.set_sensitivity(sensitivity),
                Command::Input(name) => input.switch(name),
                Command::Output(name) => output.switch(name),
                Command::Voice { key, name, queue } => mixer.add(key, name, queue),
            }
            next = commands.try_recv().ok();
        }
        keep_open(&mut output, &mut input);

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
    fn stream_perdido_reabre_no_padrao() {
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
        assert_eq!(*asked.borrow(), [Some("fone".to_string()), None]);
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
