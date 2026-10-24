use crate::highlight::Hl;
use ratatui::style::{Color, Modifier, Style};

pub const TEXT: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
pub const SUBTEXT: Color = Color::Rgb(0xa6, 0xad, 0xc8);
pub const OVERLAY: Color = Color::Rgb(0x6c, 0x70, 0x86);
pub const SURFACE: Color = Color::Rgb(0x31, 0x32, 0x44);
pub const BASE: Color = Color::Rgb(0x1e, 0x1e, 0x2e);
pub const MANTLE: Color = Color::Rgb(0x18, 0x18, 0x25);
pub const MAUVE: Color = Color::Rgb(0xcb, 0xa6, 0xf7);
pub const LAVENDER: Color = Color::Rgb(0xb4, 0xbe, 0xfe);
pub const BLUE: Color = Color::Rgb(0x89, 0xb4, 0xfa);
pub const SAPPHIRE: Color = Color::Rgb(0x74, 0xc7, 0xec);
pub const TEAL: Color = Color::Rgb(0x94, 0xe2, 0xd5);
pub const GREEN: Color = Color::Rgb(0xa6, 0xe3, 0xa1);
pub const YELLOW: Color = Color::Rgb(0xf9, 0xe2, 0xaf);
pub const PEACH: Color = Color::Rgb(0xfa, 0xb3, 0x87);
pub const RED: Color = Color::Rgb(0xf3, 0x8b, 0xa8);
pub const PINK: Color = Color::Rgb(0xf5, 0xc2, 0xe7);
pub const ADD_BG: Color = Color::Rgb(0x26, 0x3a, 0x2f);
pub const DEL_BG: Color = Color::Rgb(0x42, 0x26, 0x31);
pub const FOCUS_BG: Color = Color::Rgb(0x3a, 0x34, 0x52);

pub fn hl(h: Hl) -> Style {
    let s = Style::default();
    match h {
        Hl::Keyword => s.fg(MAUVE),
        Hl::Function => s.fg(BLUE),
        Hl::Type => s.fg(YELLOW),
        Hl::Constructor => s.fg(PEACH),
        Hl::String => s.fg(GREEN),
        Hl::Number => s.fg(PEACH),
        Hl::Comment => s.fg(OVERLAY).add_modifier(Modifier::ITALIC),
        Hl::Operator => s.fg(SAPPHIRE),
        Hl::Module => s.fg(LAVENDER),
        Hl::Param => s.fg(PINK),
        Hl::Punct => s.fg(SUBTEXT),
    }
}

pub fn dim() -> Style {
    Style::default().fg(OVERLAY)
}

pub fn text() -> Style {
    Style::default().fg(TEXT)
}
