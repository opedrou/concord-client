//! Estado da call ativa: conexão, quem está na sala e quem fala. Sem widgets.
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iced::{Point, Size};

use crate::api::{self, Participant};
use crate::audio::{Engine, MicFrames};
use crate::call::{Call, End, Live};
use crate::config::Config;

/// Quanto o anel de fala fica aceso depois que a pessoa cala, como na web.
const SPEAKING_HOLD: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
    Connecting,
    Connected,
    Reconnecting(u32),
}

/// `Msg` precisa de `Clone + Debug`, e `MicFrames` não tem nenhum dos dois: a
/// mensagem leva o motor e os blocos do mic numa caixa que se abre uma vez.
#[derive(Clone)]
pub struct Boot(Arc<Mutex<Option<(Engine, MicFrames)>>>);

impl Boot {
    pub fn new((engine, mic): (Engine, MicFrames)) -> Self {
        Self(Arc::new(Mutex::new(Some((engine, mic)))))
    }

    pub fn take(&self) -> Option<(Engine, MicFrames)> {
        self.0.lock().ok()?.take()
    }
}

impl fmt::Debug for Boot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Boot")
    }
}

pub struct ActiveCall {
    /// Separa esta call da anterior: eventos atrasados de quem saiu são ignorados.
    pub id: u64,
    pub slug: String,
    pub status: Status,
    /// `None` enquanto o motor de áudio ainda abre.
    pub call: Option<Call>,
    pub engine: Option<Engine>,
    /// Sem duplicados e em ordem de nome. `None` até a primeira lista da sala.
    pub live: Option<Vec<Live>>,
    /// Nome -> última vez que falou. Quem passou de [`SPEAKING_HOLD`] sai.
    speaking: HashMap<String, Instant>,
    /// Pessoa com o painel de volume aberto.
    pub volume_panel: Option<String>,
}

impl ActiveCall {
    pub fn new(id: u64, slug: String) -> Self {
        Self {
            id,
            slug,
            status: Status::Connecting,
            call: None,
            engine: None,
            live: None,
            speaking: HashMap::new(),
            volume_panel: None,
        }
    }

    pub fn attach(&mut self, call: Call, engine: Engine) {
        self.call = Some(call);
        self.engine = Some(engine);
    }

    pub fn leave(&self) {
        if let Some(call) = &self.call {
            call.leave();
        }
    }

    /// Mic e fone da config valem pra sala. `Call::set_mic_muted` já muta o motor.
    pub fn apply_mute(&self, config: &Config) {
        if let Some(call) = &self.call {
            call.set_mic_muted(config.mic_muted);
        }
        if let Some(engine) = &self.engine {
            engine.set_deafened(config.deafened);
        }
    }

    pub fn participants(&mut self, list: Vec<Live>, now: Instant) {
        // Mesma regra de nome repetido da sidebar: `api::occupants`.
        let flat: Vec<Participant> = list
            .iter()
            .map(|l| Participant {
                identity: l.identity.clone(),
                name: l.name.clone(),
                muted: l.muted,
                camera: l.camera,
                screen_share: l.screen_share,
            })
            .collect();
        let order: Vec<&str> = api::occupants(&flat)
            .into_iter()
            .map(|p| p.identity.as_str())
            .collect();
        let mut by_identity: HashMap<&str, &Live> =
            list.iter().map(|l| (l.identity.as_str(), l)).collect();
        let live: Vec<Live> = order
            .into_iter()
            .filter_map(|id| by_identity.remove(id).cloned())
            .collect();

        let names: HashSet<&str> = live.iter().map(name_of).collect();
        self.speaking.retain(|n, _| names.contains(n.as_str()));
        for l in live.iter().filter(|l| l.speaking) {
            self.speaking.insert(name_of(l).to_owned(), now);
        }
        self.live = Some(live);
    }

    pub fn tick(&mut self, now: Instant) {
        self.speaking
            .retain(|_, t| now.saturating_duration_since(*t) < SPEAKING_HOLD);
    }

    pub fn speaking(&self, name: &str) -> bool {
        self.speaking.contains_key(name)
    }

    /// Pede quadros de relógio só enquanto algum anel pode estar por apagar.
    pub fn holding(&self) -> bool {
        !self.speaking.is_empty()
    }
}

pub fn name_of(l: &Live) -> &str {
    if l.name.is_empty() {
        &l.identity
    } else {
        &l.name
    }
}

/// Aviso curto na home quando a call acaba por outro motivo que não sair.
pub fn end_notice(end: End) -> Option<&'static str> {
    match end {
        End::Left | End::Unauthorized => None,
        End::Duplicate => Some("Você entrou nessa call de outro lugar."),
        End::Removed => Some("Você foi removido da call."),
        End::RoomClosed => Some("A sala foi fechada."),
        End::GaveUp => Some("Não foi possível reconectar à call."),
    }
}

pub const GAP: f32 = 14.0;
const PAD_X: f32 = 20.0;
const PAD_TOP: f32 = 6.0;
const PAD_BOTTOM: f32 = 14.0;
/// Margem entre o painel de volume e a borda do palco.
const PANEL_MARGIN: f32 = 8.0;

/// Colunas e fileiras da grade, nas contagens do site (2 colunas até 4, 3 até 9).
pub fn grid(n: usize) -> (usize, usize) {
    let cols = match n {
        0 | 1 => 1,
        2..=4 => 2,
        5..=9 => 3,
        _ => 4,
    };
    (cols, n.div_ceil(cols).max(1))
}

/// Tile em 16:9 que cabe na área do palco. Em janela alta manda a largura; em
/// janela baixa manda a altura.
// ponytail: sem rolagem, de 25 pessoas pra cima os tiles ficam minúsculos.
pub fn tile_size(area: Size, n: usize) -> Size {
    let (cols, rows) = grid(n);
    let by_width = (area.width - 2.0 * PAD_X - GAP * (cols - 1) as f32) / cols as f32;
    let by_height = (area.height - PAD_TOP - PAD_BOTTOM - GAP * (rows - 1) as f32) / rows as f32;
    let h = (by_width * 9.0 / 16.0).min(by_height).max(40.0);
    Size::new(h * 16.0 / 9.0, h)
}

/// Onde abrir o painel de volume: centrado no tile clicado, sem sair do palco. O `iced`
/// não informa a posição do clique, então a âncora é o tile.
pub fn panel_origin(area: Size, n: usize, index: usize, panel: Size) -> Point {
    let tile = tile_size(area, n);
    let (cols, rows) = grid(n);
    let block = |count: usize, side: f32| count as f32 * side + GAP * (count - 1) as f32;
    let left = (area.width - block(cols, tile.width)) / 2.0;
    let top = (area.height - block(rows, tile.height)) / 2.0;
    let x = left + (index % cols) as f32 * (tile.width + GAP) + (tile.width - panel.width) / 2.0;
    let y = top + (index / cols) as f32 * (tile.height + GAP) + (tile.height - panel.height) / 2.0;
    let fit = |value: f32, free: f32| value.min(free - PANEL_MARGIN).max(PANEL_MARGIN);
    Point::new(
        fit(x, area.width - panel.width),
        fit(y, area.height - panel.height),
    )
}

#[cfg(test)]
pub fn test_live(name: &str, speaking: bool) -> Live {
    Live {
        identity: format!("{name}__1"),
        name: name.into(),
        is_local: false,
        muted: false,
        camera: false,
        screen_share: false,
        speaking,
    }
}

#[cfg(test)]
mod tests {
    use super::test_live as live;
    use super::*;

    #[test]
    fn painel_abre_em_cima_do_tile_clicado() {
        let area = Size::new(900.0, 600.0);
        let panel = Size::new(280.0, 100.0);
        let esquerda = panel_origin(area, 4, 0, panel);
        let direita = panel_origin(area, 4, 1, panel);
        let embaixo = panel_origin(area, 4, 2, panel);
        assert!(esquerda.x < direita.x);
        assert_eq!(esquerda.y, direita.y);
        assert!(esquerda.y < embaixo.y);
        assert_eq!(esquerda.x, embaixo.x);
    }

    #[test]
    fn painel_nao_sai_do_palco_com_tile_pequeno() {
        let area = Size::new(500.0, 300.0);
        let panel = Size::new(280.0, 100.0);
        for index in 0..16 {
            let p = panel_origin(area, 16, index, panel);
            assert!(p.x >= PANEL_MARGIN && p.x + panel.width <= area.width - PANEL_MARGIN + 0.01);
            assert!(p.y >= PANEL_MARGIN && p.y + panel.height <= area.height - PANEL_MARGIN + 0.01);
        }
    }

    #[test]
    fn nome_repetido_vira_um_e_a_lista_fica_em_ordem() {
        let mut c = ActiveCall::new(1, "call".into());
        let mut fantasma = live("ana", false);
        fantasma.identity = "ana__2".into();
        c.participants(
            vec![
                live("Zé", false),
                live("ana", false),
                fantasma,
                live("Bia", false),
            ],
            Instant::now(),
        );
        let nomes: Vec<_> = c.live.unwrap().iter().map(|l| l.name.clone()).collect();
        assert_eq!(nomes, ["ana", "Bia", "Zé"]);
    }

    #[test]
    fn anel_acende_na_hora_e_apaga_depois_de_400_ms() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut c = ActiveCall::new(1, "call".into());
        c.participants(vec![live("Ana", true)], t0);
        assert!(c.speaking("Ana") && c.holding());

        c.participants(vec![live("Ana", false)], ms(100));
        c.tick(ms(399));
        assert!(c.speaking("Ana"), "segura durante o hold");
        c.tick(ms(400));
        assert!(!c.speaking("Ana") && !c.holding());
    }

    #[test]
    fn falar_de_novo_dentro_do_hold_renova() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut c = ActiveCall::new(1, "call".into());
        c.participants(vec![live("Ana", true)], t0);
        c.participants(vec![live("Ana", true)], ms(300));
        c.tick(ms(600));
        assert!(c.speaking("Ana"));
        c.tick(ms(700));
        assert!(!c.speaking("Ana"));
    }

    #[test]
    fn quem_sai_da_sala_perde_o_anel() {
        let t0 = Instant::now();
        let mut c = ActiveCall::new(1, "call".into());
        c.participants(vec![live("Ana", true), live("Bia", true)], t0);
        c.participants(vec![live("Bia", false)], t0);
        assert!(!c.speaking("Ana") && c.speaking("Bia"));
    }

    #[test]
    fn grade_segue_as_contagens_do_site() {
        let cols: Vec<_> = (0..=10).map(|n| grid(n).0).collect();
        assert_eq!(cols, [1, 1, 2, 2, 2, 3, 3, 3, 3, 3, 4]);
        assert_eq!(grid(4), (2, 2));
        assert_eq!(grid(9), (3, 3));
        assert_eq!(grid(10), (4, 3));
    }

    #[test]
    fn tile_cabe_no_palco_em_16_por_9() {
        for n in [1, 2, 4, 9, 12] {
            let area = Size::new(900.0, 560.0);
            let t = tile_size(area, n);
            let (cols, rows) = grid(n);
            assert!((t.width / t.height - 16.0 / 9.0).abs() < 0.01);
            assert!(
                t.width * cols as f32 + GAP * (cols - 1) as f32 <= area.width - 2.0 * PAD_X + 0.5
            );
            assert!(t.height * rows as f32 + GAP * (rows - 1) as f32 <= area.height);
        }
    }

    #[test]
    fn so_o_ultimo_motivo_de_fim_certo_gera_aviso() {
        assert_eq!(end_notice(End::Left), None);
        assert_eq!(end_notice(End::Unauthorized), None);
        assert!(end_notice(End::RoomClosed).is_some());
        assert!(end_notice(End::GaveUp).is_some());
    }
}
