//! Ícone do app: as três barras da marca, brancas, num círculo roxo, como o favicon do site.
//! A marca solta, roxa sem fundo, sumia no painel escuro e ficava fina em 16 px.
//! `assets/icon.png` vai na janela e no menu do Linux; `assets/icon.ico` vai no `.exe` pelo
//! `build.rs`. Regerar: `cargo test -- --ignored gera_icone`.
use iced::window;

pub const PNG: &[u8] = include_bytes!("../assets/icon.png");

pub fn window() -> Option<window::Icon> {
    window::icon::from_file_data(PNG, None).ok()
}

/// Barras do `mark.svg` (x, y, largura, altura) no quadro 100x100, todas com ponta redonda.
const BARS: [(f32, f32, f32, f32); 3] = [
    (22.0, 44.0, 12.0, 34.0),
    (44.0, 24.0, 12.0, 54.0),
    (66.0, 38.0, 12.0, 40.0),
];
const ROXO: [u8; 3] = [0x9d, 0x4e, 0xdd];

/// Quanto do ponto (x, y), no quadro 100x100, cai dentro de uma barra: 0 ou 1.
fn inside(x: f32, y: f32) -> bool {
    BARS.iter().any(|&(bx, by, w, h)| {
        let r = w / 2.0;
        let cx = bx + r;
        // Cápsula: retângulo do meio mais um semicírculo em cima e outro embaixo.
        let cy = y.clamp(by + r, by + h - r);
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    })
}

/// Tamanho das barras dentro do círculo.
const ESCALA: f32 = 1.0;

/// Ícone em `size` x `size` RGBA, com 4x4 amostras por pixel. Quadro 100x100: círculo de
/// raio 50 no centro, e as barras na `ESCALA`, em volta do centro delas, (50, 51), postas no meio.
#[cfg_attr(not(test), allow(dead_code))]
fn render(size: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut fundo, mut barra) = (0u32, 0u32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = (px as f32 + (sx as f32 + 0.5) / 4.0) * 100.0 / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / 4.0) * 100.0 / size as f32;
                    if (x - 50.0).powi(2) + (y - 50.0).powi(2) > 2500.0 {
                        continue;
                    }
                    if inside(50.0 + (x - 50.0) / ESCALA, 51.0 + (y - 50.0) / ESCALA) {
                        barra += 1;
                    } else {
                        fundo += 1;
                    }
                }
            }
            let cobertura = fundo + barra;
            let cor = |c: u8| match cobertura {
                0 => 0,
                n => ((u32::from(c) * fundo + 255 * barra) / n) as u8,
            };
            out.extend(ROXO.map(cor));
            out.push((cobertura * 255 / 16) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn barra_branca_no_circulo_roxo_e_canto_transparente() {
        let px = render(64);
        let rgba = |x: usize, y: usize| &px[(y * 64 + x) * 4..][..4];
        assert_eq!(rgba(0, 0)[3], 0, "canto transparente");
        assert_eq!(rgba(32, 40), [255, 255, 255, 255], "meio da barra do meio");
        assert_eq!(
            rgba(32, 4),
            [0x9d, 0x4e, 0xdd, 255],
            "círculo acima das barras"
        );
    }

    #[test]
    fn icone_da_janela_carrega() {
        assert!(window().is_some());
    }

    #[test]
    #[ignore]
    fn gera_icone() {
        use image::codecs::ico::{IcoEncoder, IcoFrame};
        let png = image::RgbaImage::from_raw(256, 256, render(256)).unwrap();
        png.save("assets/icon.png").unwrap();
        let frames: Vec<IcoFrame> = [16u32, 24, 32, 48, 64, 128, 256]
            .iter()
            .map(|&s| IcoFrame::as_png(&render(s), s, s, image::ExtendedColorType::Rgba8).unwrap())
            .collect();
        let file = std::fs::File::create("assets/icon.ico").unwrap();
        IcoEncoder::new(file).encode_images(&frames).unwrap();
    }
}
