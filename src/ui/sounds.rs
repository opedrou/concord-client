//! Quando tocar os sons de entrada, saída e transmissão. Mesmas regras do
//! `JoinLeaveSounds.tsx` do site; só a decisão, quem toca é o [`crate::audio::Engine`].
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use crate::{audio::Effect, call::Live};

/// Mínimo entre dois sons iguais: várias entradas juntas viram um som só.
const MIN_GAP: Duration = Duration::from_millis(350);
/// Folga depois da primeira lista: a sala entrega o estado atual, e isso não é novidade.
const READY_DELAY: Duration = Duration::from_millis(1200);

#[derive(Default)]
pub struct Cues {
    /// Última lista vista: entrada, saída e tela se medem contra ela.
    known: Option<Vec<Live>>,
    ready_at: Option<Instant>,
    last: HashMap<Effect, Instant>,
    joined: bool,
}

impl Cues {
    /// Minha própria entrada, uma vez por call: reconectar não repete, e não espera a
    /// folga (essa existe pra não anunciar quem já estava).
    pub fn connected(&mut self, now: Instant) -> Option<Effect> {
        let first = !std::mem::replace(&mut self.joined, true);
        first
            .then_some(Effect::Join)
            .filter(|&e| self.allow(e, now))
    }

    /// Lista nova da sala. A primeira só vira referência; nada toca.
    pub fn roster(&mut self, list: &[Live], now: Instant) -> Vec<Effect> {
        let ready_at = *self.ready_at.get_or_insert(now + READY_DELAY);
        let before = self.known.replace(list.to_vec());
        let Some(before) = before.filter(|_| now >= ready_at) else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        diff(&before, list)
            .into_iter()
            .filter(|&e| seen.insert(e) && self.allow(e, now))
            .collect()
    }

    fn allow(&mut self, effect: Effect, now: Instant) -> bool {
        let ok = self
            .last
            .get(&effect)
            .is_none_or(|&t| now.duration_since(t) >= MIN_GAP);
        if ok {
            self.last.insert(effect, now);
        }
        ok
    }
}

/// O que mudou entre duas listas. Eu mesmo só conto transmitindo: a entrada e a saída
/// da própria pessoa vêm de [`Cues::connected`] e do fim da call.
fn diff(before: &[Live], now: &[Live]) -> Vec<Effect> {
    let mut out = Vec::new();
    for n in now {
        match before.iter().find(|b| b.identity == n.identity) {
            None if !n.is_local => {
                out.push(Effect::Join);
                if n.screen_share {
                    out.push(Effect::StreamStart);
                }
            }
            Some(b) if b.screen_share != n.screen_share => out.push(if n.screen_share {
                Effect::StreamStart
            } else {
                Effect::StreamStop
            }),
            _ => {}
        }
    }
    for b in before {
        if !b.is_local && !now.iter().any(|n| n.identity == b.identity) {
            out.push(Effect::Leave);
            // Sair transmitindo não gera "parou": sem isto a tela sumiria em silêncio.
            if b.screen_share {
                out.push(Effect::StreamStop);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(id: &str, local: bool, share: bool) -> Live {
        Live {
            identity: id.into(),
            name: id.into(),
            is_local: local,
            muted: false,
            camera: false,
            screen_share: share,
            speaking: false,
            not_hearing_me: false,
        }
    }

    const T: fn(u64) -> Duration = Duration::from_millis;

    /// Call com eu e a ana, já passada a folga da primeira lista.
    fn pronta(t0: Instant) -> Cues {
        let mut c = Cues::default();
        assert!(
            c.roster(&[live("eu", true, false), live("ana", false, true)], t0)
                .is_empty()
        );
        c
    }

    #[test]
    fn lista_inicial_nao_toca_nem_transmissao_que_ja_estava_no_ar() {
        let t0 = Instant::now();
        let mut c = Cues::default();
        let l = [live("eu", true, false), live("ana", false, true)];
        assert!(c.roster(&l, t0).is_empty());
        assert!(c.roster(&l, t0 + T(100)).is_empty());
    }

    #[test]
    fn dentro_da_folga_so_atualiza_a_referencia() {
        let t0 = Instant::now();
        let mut c = pronta(t0);
        let l = [
            live("eu", true, false),
            live("ana", false, true),
            live("bia", false, false),
        ];
        assert!(c.roster(&l, t0 + T(500)).is_empty());
        // A bia já é conhecida: sair depois da folga toca.
        let sem_bia = [live("eu", true, false), live("ana", false, true)];
        assert_eq!(c.roster(&sem_bia, t0 + READY_DELAY + T(1)), [Effect::Leave]);
    }

    #[test]
    fn outra_pessoa_entra_e_sai() {
        let t0 = Instant::now() + T(0);
        let mut c = pronta(t0);
        let t = t0 + READY_DELAY;
        let com_bia = [
            live("eu", true, false),
            live("ana", false, true),
            live("bia", false, false),
        ];
        assert_eq!(c.roster(&com_bia, t), [Effect::Join]);
        let sem_bia = [live("eu", true, false), live("ana", false, true)];
        assert_eq!(c.roster(&sem_bia, t + T(1000)), [Effect::Leave]);
    }

    #[test]
    fn entrar_ja_transmitindo_toca_entrada_e_transmissao() {
        let t0 = Instant::now();
        let mut c = pronta(t0);
        let l = [
            live("eu", true, false),
            live("ana", false, true),
            live("bia", false, true),
        ];
        assert_eq!(
            c.roster(&l, t0 + READY_DELAY),
            [Effect::Join, Effect::StreamStart]
        );
    }

    #[test]
    fn eu_ou_outra_pessoa_comeca_e_para_de_transmitir() {
        let t0 = Instant::now();
        let mut c = pronta(t0);
        let t = t0 + READY_DELAY;
        let eu_transmito = [live("eu", true, true), live("ana", false, true)];
        assert_eq!(c.roster(&eu_transmito, t), [Effect::StreamStart]);
        let tudo_parado = [live("eu", true, false), live("ana", false, false)];
        // Dois pararam juntos: um som só.
        assert_eq!(c.roster(&tudo_parado, t + T(1000)), [Effect::StreamStop]);
        let ana_volta = [live("eu", true, false), live("ana", false, true)];
        assert_eq!(c.roster(&ana_volta, t + T(2000)), [Effect::StreamStart]);
    }

    #[test]
    fn sair_transmitindo_toca_saida_e_fim_da_transmissao() {
        let t0 = Instant::now();
        let mut c = pronta(t0);
        let sozinho = [live("eu", true, false)];
        assert_eq!(
            c.roster(&sozinho, t0 + READY_DELAY),
            [Effect::Leave, Effect::StreamStop]
        );
    }

    #[test]
    fn varios_juntos_viram_um_som_e_o_intervalo_segura_a_rajada() {
        let t0 = Instant::now();
        let mut c = pronta(t0);
        let t = t0 + READY_DELAY;
        let tres = [
            live("eu", true, false),
            live("ana", false, true),
            live("bia", false, false),
            live("cai", false, false),
        ];
        assert_eq!(c.roster(&tres, t), [Effect::Join]);
        let quatro = [&tres[..], &[live("duda", false, false)]].concat();
        assert!(c.roster(&quatro, t + T(100)).is_empty(), "dentro de 350 ms");
        let cinco = [&quatro[..], &[live("edu", false, false)]].concat();
        assert_eq!(c.roster(&cinco, t + T(500)), [Effect::Join]);
    }

    #[test]
    fn minha_entrada_toca_uma_vez_so() {
        let t0 = Instant::now();
        let mut c = Cues::default();
        assert_eq!(c.connected(t0), Some(Effect::Join));
        assert_eq!(c.connected(t0 + T(5000)), None, "reconexão");
    }
}
