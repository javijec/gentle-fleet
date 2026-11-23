use gpui::{hsla, Hsla};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub canvas: Hsla,
    pub panel: Hsla,
    pub elevated: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub accent: Hsla,
    pub accent_text: Hsla,
    pub focus: Hsla,
}

pub const SPACING: [f32; 6] = [4.0, 8.0, 12.0, 16.0, 24.0, 32.0];
pub const RADIUS: f32 = 8.0;
pub const BORDER_WIDTH: f32 = 1.0;
pub const FONT_SIZE_BODY: f32 = 14.0;
pub const FONT_SIZE_TITLE: f32 = 24.0;

impl ThemeMode {
    pub fn palette(self) -> Palette {
        let (canvas, panel, elevated, border, text, muted, accent, accent_text, focus) = match self
        {
            Self::Dark => (
                0x111318, 0x191c22, 0x22262e, 0x303640, 0xf0f2f5, 0xa0a8b5, 0x8bb8ff, 0x111318,
                0xb5d1ff,
            ),
            Self::Light => (
                0xf5f7fa, 0xffffff, 0xeaf0f7, 0xd7dde6, 0x202733, 0x5e6877, 0x245fa8, 0xffffff,
                0x174f91,
            ),
        };

        Palette {
            canvas: color(canvas),
            panel: color(panel),
            elevated: color(elevated),
            border: color(border),
            text: color(text),
            muted: color(muted),
            accent: color(accent),
            accent_text: color(accent_text),
            focus: color(focus),
        }
    }
}

fn color(rgb: u32) -> Hsla {
    let r = ((rgb >> 16) & 0xff) as f32 / 255.0;
    let g = ((rgb >> 8) & 0xff) as f32 / 255.0;
    let b = (rgb & 0xff) as f32 / 255.0;
    hsla(r, g, b, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_modes_provide_distinct_canvas_and_focus_tokens() {
        let dark = ThemeMode::Dark.palette();
        let light = ThemeMode::Light.palette();

        assert_ne!(dark.canvas, light.canvas);
        assert_ne!(dark.focus, light.focus);
        assert!(SPACING.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
