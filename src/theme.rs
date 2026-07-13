//! Color themes for the TUI. Themes are simple palettes selected by name.

use ratatui::style::Color;

/// A named color palette.
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: &'static str,
    /// Primary accent (selected rows, active tab).
    pub accent: Color,
    /// Secondary accent (headers, labels).
    pub secondary: Color,
    /// Normal foreground text.
    pub fg: Color,
    /// Dimmed/help text.
    pub dim: Color,
    /// "Update available" / warning color.
    pub warn: Color,
    /// "Up to date" / success color.
    pub ok: Color,
    /// Error color.
    pub err: Color,
    /// Border color.
    pub border: Color,
}

impl Theme {
    /// All built-in themes, in cycle order.
    pub fn all() -> &'static [&'static str] {
        &["midnight", "solar", "gruvbox", "mono"]
    }

    /// Look up a theme by name, falling back to `midnight`.
    pub fn by_name(name: &str) -> Theme {
        match name {
            "solar" => Theme::solar(),
            "gruvbox" => Theme::gruvbox(),
            "mono" => Theme::mono(),
            _ => Theme::midnight(),
        }
    }

    /// Return the next theme name in cycle order.
    pub fn next(name: &str) -> &'static str {
        let all = Theme::all();
        let idx = all.iter().position(|n| *n == name).unwrap_or(0);
        all[(idx + 1) % all.len()]
    }

    fn midnight() -> Theme {
        Theme {
            name: "midnight",
            accent: Color::Rgb(122, 162, 247),
            secondary: Color::Rgb(158, 206, 106),
            fg: Color::Rgb(192, 202, 245),
            dim: Color::Rgb(86, 95, 137),
            warn: Color::Rgb(224, 175, 104),
            ok: Color::Rgb(158, 206, 106),
            err: Color::Rgb(247, 118, 142),
            border: Color::Rgb(65, 72, 104),
        }
    }

    fn solar() -> Theme {
        Theme {
            name: "solar",
            accent: Color::Rgb(38, 139, 210),
            secondary: Color::Rgb(133, 153, 0),
            fg: Color::Rgb(131, 148, 150),
            dim: Color::Rgb(88, 110, 117),
            warn: Color::Rgb(181, 137, 0),
            ok: Color::Rgb(133, 153, 0),
            err: Color::Rgb(220, 50, 47),
            border: Color::Rgb(88, 110, 117),
        }
    }

    fn gruvbox() -> Theme {
        Theme {
            name: "gruvbox",
            accent: Color::Rgb(131, 165, 152),
            secondary: Color::Rgb(184, 187, 38),
            fg: Color::Rgb(235, 219, 178),
            dim: Color::Rgb(146, 131, 116),
            warn: Color::Rgb(250, 189, 47),
            ok: Color::Rgb(184, 187, 38),
            err: Color::Rgb(251, 73, 52),
            border: Color::Rgb(80, 73, 69),
        }
    }

    fn mono() -> Theme {
        Theme {
            name: "mono",
            accent: Color::White,
            secondary: Color::Gray,
            fg: Color::Gray,
            dim: Color::DarkGray,
            warn: Color::White,
            ok: Color::Gray,
            err: Color::White,
            border: Color::DarkGray,
        }
    }
}
