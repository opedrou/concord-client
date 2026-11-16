//! Conta os desenhos da janela. O iced só solta da GPU uma imagem trocada no fim de um
//! desenho, e janela escondida não desenha: minimizada no Windows ela vai a 0x0, e no Wayland
//! o compositor para de pedir quadro. Quem sobe quadro por `image::allocate` espera um desenho
//! depois da última subida, senão cada quadro fica na GPU até a janela voltar (11 GB em 1 min
//! assistindo 1080p).
//!
//! Por thread: `update`, `view` e o desenho rodam todos na thread da janela.

use std::cell::Cell;

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{Tree, Widget};
use iced::widget::stack;
use iced::{Element, Length, Rectangle, Size, mouse};

thread_local! {
    static DRAWS: Cell<u64> = const { Cell::new(0) };
}

/// Desenhos até agora.
pub fn count() -> u64 {
    DRAWS.get()
}

/// Houve desenho depois da marca, ou nada subiu ainda.
pub fn since(mark: Option<u64>) -> bool {
    mark.is_none_or(|m| count() > m)
}

fn bump() {
    DRAWS.set(DRAWS.get() + 1);
}

/// Põe por cima de `content` um widget sem tamanho que só conta o desenho.
// ponytail: conta o desenho da interface, não o `present`. Se o `present` falhar em laço (device
// perdido), conta sem soltar nada; aí o app já está quebrado de qualquer jeito.
pub fn probe<'a, M: 'a>(content: Element<'a, M>) -> Element<'a, M> {
    stack![content, Element::new(Probe)].into()
}

struct Probe;

impl<M, T, R: renderer::Renderer> Widget<M, T, R> for Probe {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(&mut self, _: &mut Tree, _: &R, _: &layout::Limits) -> layout::Node {
        layout::Node::new(Size::ZERO)
    }

    fn draw(
        &self,
        _: &Tree,
        _: &mut R,
        _: &T,
        _: &renderer::Style,
        _: Layout<'_>,
        _: mouse::Cursor,
        _: &Rectangle,
    ) {
        bump();
    }
}

#[cfg(test)]
pub fn draw_for_test() {
    bump();
}
