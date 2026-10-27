//! Settings: one table drives the defaults, the ini file and the settings window.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::{NAME, PLUGIN_DIR, lock};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Text,
    Int,
    Bool,
    Color,
    /// Index into the options; `Monitor` options are built at runtime.
    Choice(&'static [&'static str]),
    Monitor,
}

pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    pub default: &'static str,
}

pub const CORNERS: &[&str] = &["Левый верхний", "Правый верхний", "Левый нижний", "Правый нижний"];

/// Settings window columns: (title, fields).
pub const SECTIONS: &[(&str, &[Field])] = &[
    ("Общие", &[
        Field { key: "font", label: "Шрифт", kind: Kind::Text, default: "Segoe UI" },
        Field { key: "bold", label: "Жирный шрифт", kind: Kind::Bool, default: "1" },
        Field { key: "opacity", label: "Непрозрачность, %", kind: Kind::Int, default: "92" },
        Field { key: "linger", label: "Держать ник после речи, с", kind: Kind::Int, default: "15" },
        Field { key: "show_self", label: "Показывать себя", kind: Kind::Bool, default: "1" },
        Field { key: "color_bg", label: "Фон", kind: Kind::Color, default: "#1E1F22" },
        Field { key: "color_border", label: "Рамка", kind: Kind::Color, default: "#3A3D42" },
        Field { key: "color_text", label: "Текст", kind: Kind::Color, default: "#FFFFFF" },
        Field { key: "color_faded", label: "Текст (замолчал)", kind: Kind::Color, default: "#A5AAAF" },
        Field { key: "color_talk", label: "Точка: канал", kind: Kind::Color, default: "#00E676" },
        Field { key: "color_whisper", label: "Точка: шёпот", kind: Kind::Color, default: "#FFB300" },
    ]),
    ("Кто говорит", &[
        Field { key: "talk_enabled", label: "Включён", kind: Kind::Bool, default: "1" },
        Field { key: "talk_hotkey", label: "Горячая клавиша", kind: Kind::Text, default: "Ctrl+Alt+T" },
        Field { key: "talk_monitor", label: "Монитор", kind: Kind::Monitor, default: "0" },
        Field { key: "talk_corner", label: "Угол", kind: Kind::Choice(CORNERS), default: "3" },
        Field { key: "talk_x", label: "Отступ по X, px", kind: Kind::Int, default: "408" },
        Field { key: "talk_y", label: "Отступ по Y, px", kind: Kind::Int, default: "24" },
        Field { key: "talk_width", label: "Ширина, px", kind: Kind::Int, default: "440" },
        Field { key: "talk_font_px", label: "Размер шрифта, px", kind: Kind::Int, default: "27" },
        Field { key: "talk_gap", label: "Отступ шёпот/канал, px", kind: Kind::Int, default: "14" },
    ]),
    ("Список канала", &[
        Field { key: "list_enabled", label: "Включён", kind: Kind::Bool, default: "1" },
        Field { key: "list_hotkey", label: "Горячая клавиша", kind: Kind::Text, default: "Ctrl+Alt+L" },
        Field { key: "list_monitor", label: "Монитор", kind: Kind::Monitor, default: "0" },
        Field { key: "list_corner", label: "Угол", kind: Kind::Choice(CORNERS), default: "1" },
        Field { key: "list_x", label: "Отступ по X, px", kind: Kind::Int, default: "408" },
        Field { key: "list_y", label: "Отступ по Y, px", kind: Kind::Int, default: "24" },
        Field { key: "list_width", label: "Ширина, px", kind: Kind::Int, default: "320" },
        Field { key: "list_font_px", label: "Размер шрифта, px", kind: Kind::Int, default: "20" },
    ]),
];

pub fn fields() -> impl Iterator<Item = &'static Field> {
    SECTIONS.iter().flat_map(|(_, f)| f.iter())
}

#[derive(Clone)]
pub struct Config(HashMap<&'static str, String>);

pub static CONFIG: LazyLock<Mutex<Config>> = LazyLock::new(|| Mutex::new(Config::load()));

impl Config {
    fn path() -> std::path::PathBuf {
        PLUGIN_DIR.join(format!("{NAME}.ini"))
    }

    pub fn load() -> Config {
        let text = std::fs::read_to_string(Self::path()).unwrap_or_default();
        let saved: HashMap<&str, &str> =
            text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim(), v.trim())).collect();
        let cfg = Config(fields().map(|f| (f.key, saved.get(f.key).unwrap_or(&f.default).to_string())).collect());
        if saved.is_empty() {
            cfg.save();
        }
        cfg
    }

    pub fn save(&self) {
        let mut text = String::from("; Удобнее менять в TS: Настройки -> Плагины -> TS Talk Overlay -> Настройки.\n");
        for (title, fs) in SECTIONS {
            text.push_str(&format!("\n; ---- {title} ----\n"));
            for f in *fs {
                text.push_str(&format!("; {}\n{}={}\n", f.label, f.key, self.get(f.key)));
            }
        }
        let _ = std::fs::write(Self::path(), text);
    }

    pub fn get(&self, key: &str) -> &str {
        self.0.get(key).map_or("", |s| s.as_str())
    }

    pub fn set(&mut self, key: &'static str, value: String) {
        self.0.insert(key, value);
    }

    pub fn int(&self, key: &str) -> i32 {
        self.get(key).parse().unwrap_or(0)
    }

    pub fn bool(&self, key: &str) -> bool {
        self.get(key) == "1"
    }

    /// `#RRGGBB` as a GDI COLORREF (0x00BBGGRR).
    pub fn color(&self, key: &str) -> u32 {
        parse_color(self.get(key)).unwrap_or(0)
    }
}

pub fn parse_color(s: &str) -> Option<u32> {
    let rgb = u32::from_str_radix(s.trim().trim_start_matches('#'), 16).ok()?;
    Some(((rgb & 0xff) << 16) | (rgb & 0xff00) | (rgb >> 16 & 0xff))
}

pub fn format_color(c: u32) -> String {
    format!("#{:02X}{:02X}{:02X}", c & 0xff, c >> 8 & 0xff, c >> 16 & 0xff)
}

pub fn current() -> Config {
    lock(&CONFIG).clone()
}
