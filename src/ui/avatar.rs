//! Foto ou iniciais com cor derivada do nome, igual ao `lib/Avatar.tsx` da web.
use std::io::Cursor;

use ::image::{ImageReader, Limits, imageops::FilterType};
use iced::widget::{center, container, image, text};
use iced::{Color, Element, Length, border};

use super::{Msg, theme};

const PALETTE: [Color; 8] = [
    Color::from_rgb8(0x9d, 0x4e, 0xdd),
    Color::from_rgb8(0x7b, 0x2c, 0xbf),
    Color::from_rgb8(0xc7, 0x7d, 0xff),
    Color::from_rgb8(0x5a, 0x18, 0x9a),
    Color::from_rgb8(0xb5, 0x65, 0xe0),
    Color::from_rgb8(0x8e, 0x44, 0xc7),
    Color::from_rgb8(0xa0, 0x5a, 0xd6),
    Color::from_rgb8(0x6a, 0x2a, 0x9e),
];

/// Lado guardado na memória: o dobro do maior avatar da sidebar, pra telas HiDPI.
const LADO: u32 = 96;
const MAX_LADO_ORIGINAL: u32 = 8192;
const MAX_DECODIFICADO: u64 = 256 * 1024 * 1024;

/// Mesmo hash da web, em UTF-16 e 32 bits, pra mesma pessoa ter a mesma cor.
fn hash(name: &str) -> u32 {
    name.encode_utf16()
        .fold(0i32, |h, c| {
            h.wrapping_shl(5).wrapping_sub(h).wrapping_add(c.into())
        })
        .unsigned_abs()
}

fn color_for(name: &str) -> Color {
    PALETTE[hash(name) as usize % PALETTE.len()]
}

fn initials(name: &str) -> String {
    let parts: Vec<_> = name.split_whitespace().collect();
    match parts.as_slice() {
        [] => "?".to_owned(),
        [one] => one.chars().take(2).flat_map(char::to_uppercase).collect(),
        [first, .., last] => first
            .chars()
            .take(1)
            .chain(last.chars().take(1))
            .flat_map(char::to_uppercase)
            .collect(),
    }
}

pub fn view<'a>(name: &str, photo: Option<&image::Handle>, size: f32) -> Element<'a, Msg> {
    if let Some(photo) = photo {
        return image(photo.clone())
            .width(size)
            .height(size)
            .border_radius(size / 2.0)
            .into();
    }
    let color = color_for(name);
    container(center(
        text(initials(name)).size(size * 0.4).font(theme::SEMIBOLD),
    ))
    .width(Length::Fixed(size))
    .height(Length::Fixed(size))
    .style(move |_| container::Style {
        background: Some(color.into()),
        border: border::rounded(size / 2.0),
        text_color: Some(theme::FG),
        ..Default::default()
    })
    .into()
}

/// Anel verde de quem está falando. Apagado, continua ocupando os 2 px, pra nada pular.
pub fn ringed<'a>(inner: Element<'a, Msg>, size: f32, on: bool) -> Element<'a, Msg> {
    let color = if on { theme::GREEN } else { Color::TRANSPARENT };
    container(inner)
        .padding(2)
        .style(move |_| container::Style {
            border: border::rounded((size + 4.0) / 2.0).color(color).width(2),
            ..Default::default()
        })
        .into()
}

/// Corta no centro e reduz, como o `object-fit: cover` da web. Os limites barram
/// imagem pequena em bytes e gigante na memória.
fn decode(bytes: &[u8]) -> Option<(image::Handle, Color)> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_LADO_ORIGINAL);
    limits.max_image_height = Some(MAX_LADO_ORIGINAL);
    limits.max_alloc = Some(MAX_DECODIFICADO);
    reader.limits(limits);
    let rgba = reader
        .decode()
        .ok()?
        .resize_to_fill(LADO, LADO, FilterType::Triangle)
        .into_rgba8();
    let (w, h) = rgba.dimensions();
    let tint = dominant(&rgba);
    Some((image::Handle::from_rgba(w, h, rgba.into_raw()), tint))
}

/// Cor que domina a foto, pro fundo do tile. Agrupa em 4 bits por canal e pesa pela saturação
/// ao quadrado: fundo branco ou cinza não ganha do tema colorido (o pato amarelo vira amarelo).
fn dominant(img: &::image::RgbaImage) -> Color {
    let mut bins = vec![(0f32, [0u32; 4]); 4096];
    for px in img.pixels().filter(|p| p[3] > 128) {
        let [r, g, b, _] = px.0;
        let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
        let sat = f32::from(hi - lo) / 255.0;
        let bin =
            &mut bins[usize::from(r >> 4) << 8 | usize::from(g >> 4) << 4 | usize::from(b >> 4)];
        bin.0 += sat * sat + 0.01;
        for (acc, c) in bin.1.iter_mut().zip([r, g, b]) {
            *acc += u32::from(c);
        }
        bin.1[3] += 1;
    }
    let Some((_, [r, g, b, n])) = bins.into_iter().max_by(|a, b| a.0.total_cmp(&b.0)) else {
        return theme::BG2;
    };
    if n == 0 {
        return theme::BG2;
    }
    Color::from_rgb8((r / n) as u8, (g / n) as u8, (b / n) as u8)
}

pub async fn decode_off_thread(bytes: Vec<u8>) -> Option<(image::Handle, Color)> {
    tokio::task::spawn_blocking(move || decode(&bytes))
        .await
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::image::{ImageFormat, RgbaImage};

    /// Valores tirados do `hashString` da web, rodado no Node.
    #[test]
    fn hash_igual_ao_da_web() {
        assert_eq!(hash("Opedro"), 1926721379);
        assert_eq!(hash("ação"), 3118866);
        assert_eq!(hash("Zé da Silva"), 308066735);
    }

    #[test]
    fn iniciais_como_na_web() {
        assert_eq!(initials("Opedro"), "OP");
        assert_eq!(initials("zé da silva"), "ZS");
        assert_eq!(initials("é"), "É");
        assert_eq!(initials("   "), "?");
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        RgbaImage::new(w, h)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn avatar_vira_quadrado_pequeno() {
        let (handle, _) = decode(&png(300, 100)).unwrap();
        assert!(matches!(
            handle,
            image::Handle::Rgba {
                width: LADO,
                height: LADO,
                ..
            }
        ));
    }

    #[test]
    fn cor_dominante_ignora_fundo_branco() {
        // Metade branca, um quarto amarelo, um quarto cinza: tem que sair o amarelo.
        let img = RgbaImage::from_fn(4, 4, |x, y| match (x < 2, y < 2) {
            (true, _) => ::image::Rgba([255, 255, 255, 255]),
            (false, true) => ::image::Rgba([255, 216, 69, 255]),
            _ => ::image::Rgba([128, 128, 128, 255]),
        });
        assert_eq!(dominant(&img), Color::from_rgb8(255, 216, 69));
    }

    #[test]
    fn imagem_invalida_ou_gigante_cai_nas_iniciais() {
        assert!(decode(b"nao e imagem").is_none());
        assert!(decode(&png(MAX_LADO_ORIGINAL + 1, 1)).is_none());
    }
}
