//! ANSI terminal styling, color palettes, and badge formatting.
//!
//! Provides:
//! - 8 standard ANSI foreground and high-intensity background colors.
//! - LRU rotating color allocation palette for dynamically seen log tags.
//! - Static preset color assignments for known system tags.
//! - High-contrast log level badges (`V`, `D`, `I`, `W`, `E`, `F`).
//! - Dimmed/gray timestamp and PID formatters.

use std::collections::HashMap;

/// Standard ANSI colors mapping to codes 0 through 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Black = 0,
    Red = 1,
    Green = 2,
    Yellow = 3,
    Blue = 4,
    Magenta = 5,
    Cyan = 6,
    White = 7,
}

/// ANSI escape code sequence to reset all text styling and colors.
pub const RESET: &str = "\x1b[0m";

/// Constructs an ANSI SGR escape sequence for the given optional foreground and background colors.
///
/// Uses standard foreground codes `30-37` and bright/high-intensity background codes `100-107`.
pub fn termcolor(fg: Option<Color>, bg: Option<Color>) -> String {
    let mut codes = Vec::new();
    if let Some(fg) = fg {
        codes.push(format!("3{}", fg as u8));
    }
    if let Some(bg) = bg {
        codes.push(format!("10{}", bg as u8));
    }
    if codes.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// Wraps `message` with ANSI color sequences if `use_color` is true.
///
/// If `use_color` is false, returns `message` unchanged without any escape codes.
pub fn colorize(message: &str, fg: Option<Color>, bg: Option<Color>, use_color: bool) -> String {
    if !use_color {
        return message.to_string();
    }
    let code = termcolor(fg, bg);
    if code.is_empty() {
        message.to_string()
    } else {
        format!("{}{}{}", code, message, RESET)
    }
}

/// Manages dynamic tag color assignment using an LRU cache.
///
/// Ensures concurrent tags receive contrasting colors while assigning
/// deterministic sticky colors to known Android system tags.
pub struct Palette {
    lru: Vec<Color>,
    known_tags: HashMap<String, Color>,
}

impl Default for Palette {
    fn default() -> Self {
        Self::new()
    }
}

impl Palette {
    /// Initializes the palette with standard sticky system tag assignments and a 6-color rotation queue.
    pub fn new() -> Self {
        let mut known_tags = HashMap::new();
        // High-frequency system tags get clean, readable static colors
        known_tags.insert("dalvikvm".to_string(), Color::White);
        known_tags.insert("Process".to_string(), Color::White);
        known_tags.insert("ActivityManager".to_string(), Color::White);
        known_tags.insert("ActivityThread".to_string(), Color::White);
        known_tags.insert("AndroidRuntime".to_string(), Color::Cyan);
        known_tags.insert("jdwp".to_string(), Color::White);
        known_tags.insert("StrictMode".to_string(), Color::White);
        known_tags.insert("DEBUG".to_string(), Color::Yellow);

        // Rotating palette colors (excluding black and white for readability on dark terminals)
        let lru = vec![
            Color::Red,
            Color::Green,
            Color::Yellow,
            Color::Blue,
            Color::Magenta,
            Color::Cyan,
        ];

        Self { lru, known_tags }
    }

    /// Allocates or retrieves the color assigned to `tag`.
    ///
    /// If `tag` is known, returns its sticky or previously allocated color.
    /// Otherwise, picks the least-recently-used color from the rotation queue,
    /// associates it with the tag, and moves that color to the back of the LRU queue.
    pub fn allocate_color(&mut self, tag: &str) -> Color {
        let color = if let Some(&c) = self.known_tags.get(tag) {
            c
        } else {
            let c = self.lru[0];
            self.known_tags.insert(tag.to_string(), c);
            c
        };

        // Bump the used color to the most-recently-used position (back of vec)
        if let Some(pos) = self.lru.iter().position(|&x| x == color) {
            let removed = self.lru.remove(pos);
            self.lru.push(removed);
        }

        color
    }
}

/// Formats a single-character log level badge (`V`, `D`, `I`, `W`, `E`, `F`).
///
/// When `use_color` is true, renders a high-contrast badge with background color:
/// - `V`: White on Black
/// - `D`: Black on Blue
/// - `I`: Black on Green
/// - `W`: Black on Yellow
/// - `E` / `F`: Black on Red
pub fn format_level(level: char, use_color: bool) -> String {
    let (badge, fg, bg) = match level {
        'V' => (" V ", Color::White, Color::Black),
        'D' => (" D ", Color::Black, Color::Blue),
        'I' => (" I ", Color::Black, Color::Green),
        'W' => (" W ", Color::Black, Color::Yellow),
        'E' => (" E ", Color::Black, Color::Red),
        'F' => (" F ", Color::Black, Color::Red),
        _ => return format!(" {} ", level),
    };

    colorize(badge, Some(fg), Some(bg), use_color)
}

/// Formats an 18-character timestamp string.
///
/// In color mode, applies dim/gray (`\x1b[90m`) so the timestamp does not distract
/// from the log level badge and tag colors.
pub fn format_timestamp(timestamp: &str, use_color: bool) -> String {
    if !use_color {
        timestamp.to_string()
    } else {
        format!("\x1b[90m{}\x1b[0m", timestamp)
    }
}

/// Formats a process ID string right-aligned to `width` columns.
///
/// In color mode, applies dim/gray (`\x1b[90m`).
pub fn format_pid(pid: &str, width: usize, use_color: bool) -> String {
    let formatted = if pid.len() <= width {
        format!("{:>width$}", pid, width = width)
    } else {
        pid.to_string()
    };
    if !use_color {
        formatted
    } else {
        format!("\x1b[90m{}\x1b[0m", formatted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_termcolor() {
        assert_eq!(termcolor(Some(Color::Red), None), "\x1b[31m");
        assert_eq!(termcolor(None, Some(Color::White)), "\x1b[107m");
        assert_eq!(
            termcolor(Some(Color::Black), Some(Color::Blue)),
            "\x1b[30;104m"
        );
        assert_eq!(termcolor(None, None), "");
    }

    #[test]
    fn test_lru_allocation() {
        let mut palette = Palette::new();
        assert_eq!(palette.allocate_color("dalvikvm"), Color::White);
        assert_eq!(palette.allocate_color("AndroidRuntime"), Color::Cyan);

        let c1 = palette.allocate_color("MyTag1");
        assert_eq!(c1, Color::Red);
        let c2 = palette.allocate_color("MyTag2");
        assert_eq!(c2, Color::Green);
        // Calling MyTag1 again should return the same color
        assert_eq!(palette.allocate_color("MyTag1"), Color::Red);
    }
}
