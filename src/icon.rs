//! Ícone do app: a marca do Concord (três barras roxas) desenhada aqui, sem SVG.
//! `assets/icon.png` vai na janela; `assets/icon.ico` vai no `.exe` pelo `build.rs`.
//! Regerar: `cargo test -- --ignored gera_icone`.
use iced::window;

pub fn window() -> Option<window::Icon> {
    window::icon::from_file_data(include_bytes!("../assets/icon.png"), None).ok()
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

/// Marca em `size` x `size` RGBA, recortada em volta das barras, com 4x4 amostras por pixel.
#[cfg_attr(not(test), allow(dead_code))]
fn render(size: u32) -> Vec<u8> {
    // Recorte quadrado de 18 a 82: as barras ocupam quase tudo, como ícone tem que ser.
    let (start, span) = (18.0, 64.0);
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let mut hits = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = start + (px as f32 + (sx as f32 + 0.5) / 4.0) * span / size as f32;
                    let y = start + (py as f32 + (sy as f32 + 0.5) / 4.0) * span / size as f32;
                    hits += u8::from(inside(x, y));
                }
            }
            out.extend_from_slice(&ROXO);
            out.push((u32::from(hits) * 255 / 16) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn barra_do_meio_aparece_e_o_canto_fica_transparente() {
        let px = render(64);
        let alpha = |x: usize, y: usize| px[(y * 64 + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0, "canto transparente");
        assert_eq!(alpha(32, 40), 255, "meio da barra do meio cheio");
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
