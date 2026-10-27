//! The two overlay windows, owned by one UI thread.
//!
//! TS callbacks only update `STATE` and post a message here; all window work happens on this
//! thread. A message-only "control" window receives those posts, hotkeys and the linger timer.

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleHandleExW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::config::{self, Config};
use crate::{STATE, State, lock, log};

const WM_APP_REFRESH: u32 = WM_APP + 1;
const WM_APP_RELOAD: u32 = WM_APP + 2;
const CLASS: &str = "TsTalkOverlay";
/// GWLP_USERDATA tags: index into `Ui::wins` + 1, or the control window.
const KIND_CONTROL: isize = 10;
const PREFIX: [&str; 2] = ["talk", "list"];

static CONTROL: AtomicUsize = AtomicUsize::new(0);
static THREAD: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

struct Ui {
    wins: [HWND; 2],
    fonts: [HFONT; 2],
    /// Toggled by the hotkeys; not saved.
    hidden: [bool; 2],
    cfg: Config,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

enum Row {
    Item { text: String, dot: u32, color: u32 },
    Gap(i32),
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Our DLL's module handle (not TS's exe), so the window class goes away with the DLL.
pub fn module() -> HINSTANCE {
    let mut m: HINSTANCE = std::ptr::null_mut();
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            module as *const () as *const u16,
            &mut m,
        );
    }
    m
}

fn post(msg: u32) {
    let hwnd = CONTROL.load(Ordering::Acquire);
    if hwnd != 0 {
        unsafe { PostMessageW(hwnd as HWND, msg, 0, 0) };
    }
}

/// Redraw after `STATE` changed. Safe to call from any thread.
pub fn refresh() {
    post(WM_APP_REFRESH);
}

/// Apply `config::CONFIG` after it changed.
pub fn reload() {
    post(WM_APP_RELOAD);
}

pub fn start() {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || run(tx));
    let _ = rx.recv_timeout(Duration::from_secs(5));
    *lock(&THREAD) = Some(handle);
}

pub fn stop() {
    post(WM_CLOSE);
    if let Some(h) = lock(&THREAD).take() {
        let _ = h.join();
    }
}

fn run(ready: mpsc::Sender<()>) {
    let class = wide(CLASS);
    unsafe {
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: module(), lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
        RegisterClassW(&wc);
        let ctrl = CreateWindowExW(0, class.as_ptr(), class.as_ptr(), 0, 0, 0, 0, 0, HWND_MESSAGE, std::ptr::null_mut(), module(), std::ptr::null());
        SetWindowLongPtrW(ctrl, GWLP_USERDATA, KIND_CONTROL);
        let mut wins = [std::ptr::null_mut(); 2];
        for (i, w) in wins.iter_mut().enumerate() {
            *w = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.as_ptr(), class.as_ptr(), WS_POPUP, 0, 0, 10, 10,
                std::ptr::null_mut(), std::ptr::null_mut(), module(), std::ptr::null(),
            );
            SetWindowLongPtrW(*w, GWLP_USERDATA, i as isize + 1);
        }
        UI.set(Some(Ui { wins, fonts: [std::ptr::null_mut(); 2], hidden: [false; 2], cfg: config::current() }));
        apply_config(ctrl);
        SetTimer(ctrl, 1, 500, None);
        CONTROL.store(ctrl as usize, Ordering::Release);
        let _ = ready.send(());

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            DispatchMessageW(&msg);
        }

        CONTROL.store(0, Ordering::Release);
        UnregisterHotKey(ctrl, 1);
        UnregisterHotKey(ctrl, 2);
        if let Some(ui) = UI.take() {
            for i in 0..2 {
                DestroyWindow(ui.wins[i]);
                DeleteObject(ui.fonts[i]);
            }
        }
        DestroyWindow(ctrl);
        UnregisterClassW(class.as_ptr(), module());
    }
}

/// "Ctrl+Alt+T", "Shift+F5" ... -> (modifiers, virtual key). Empty or unknown = no hotkey.
fn parse_hotkey(s: &str) -> Option<(u32, u32)> {
    let mut mods = 0;
    let mut vk = None;
    for part in s.split('+').map(|p| p.trim().to_uppercase()) {
        match part.as_str() {
            "CTRL" | "CONTROL" => mods |= MOD_CONTROL,
            "ALT" => mods |= MOD_ALT,
            "SHIFT" => mods |= MOD_SHIFT,
            "WIN" => mods |= MOD_WIN,
            k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => vk = Some(k.as_bytes()[0] as u32),
            k => vk = Some(0x6F + k.strip_prefix('F')?.parse::<u32>().ok().filter(|n| (1..=24).contains(n))?),
        }
    }
    Some((mods, vk?))
}

unsafe fn apply_config(ctrl: HWND) {
    let cfg = config::current();
    let wins = UI.with_borrow_mut(|ui| {
        let ui = ui.as_mut().unwrap();
        for i in 0..2 {
            unsafe {
                DeleteObject(ui.fonts[i]);
                let px = cfg.int(&format!("{}_font_px", PREFIX[i])).max(6);
                ui.fonts[i] = CreateFontW(
                    -px, 0, 0, 0, if cfg.bool("bold") { 700 } else { 400 }, 0, 0, 0,
                    DEFAULT_CHARSET as u32, OUT_DEFAULT_PRECIS as u32, CLIP_DEFAULT_PRECIS as u32,
                    CLEARTYPE_QUALITY as u32, DEFAULT_PITCH as u32,
                    wide(cfg.get("font")).as_ptr(),
                );
            }
        }
        ui.cfg = cfg.clone();
        ui.wins
    });
    let alpha = (cfg.int("opacity").clamp(10, 100) * 255 / 100) as u8;
    for (i, w) in wins.into_iter().enumerate() {
        unsafe {
            SetLayeredWindowAttributes(w, 0, alpha, LWA_ALPHA);
            UnregisterHotKey(ctrl, i as i32 + 1);
            let key = cfg.get(&format!("{}_hotkey", PREFIX[i])).to_string();
            if let Some((mods, vk)) = parse_hotkey(&key) {
                if RegisterHotKey(ctrl, i as i32 + 1, mods | MOD_NOREPEAT, vk) == 0 {
                    log(&format!("горячая клавиша {key} занята другой программой"));
                }
            } else if !key.trim().is_empty() {
                log(&format!("не понял горячую клавишу '{key}'"));
            }
        }
    }
    layout_all();
}

fn blend(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| {
        let (x, y) = ((a >> s & 0xff) as f32, (b >> s & 0xff) as f32);
        ((x * t + y * (1.0 - t)).round() as u32) << s
    };
    ch(0) | ch(8) | ch(16)
}

fn rows(i: usize, cfg: &Config, st: &State) -> Vec<Row> {
    let me = (!cfg.bool("show_self")).then_some(st.self_id);
    let (text, faded, bg) = (cfg.color("color_text"), cfg.color("color_faded"), cfg.color("color_bg"));
    if i == 1 {
        let mut people: Vec<_> = st.channel.iter().filter(|(id, _)| Some(*id) != me).collect();
        people.sort_by_key(|(_, n)| n.to_lowercase());
        return people
            .into_iter()
            .map(|(id, name)| {
                let talking = st.speakers.get(id).is_some_and(|s| s.talking && !s.whisper);
                let dot = if talking { cfg.color("color_talk") } else { blend(faded, bg, 0.35) };
                Row::Item { text: name.clone(), dot, color: text }
            })
            .collect();
    }
    // Whispers first, then a gap, then channel voice; within each group live speakers above
    // lingering ones, newest first.
    let mut out = Vec::new();
    for whisper in [true, false] {
        let mut group: Vec<_> = st.speakers.iter().filter(|(id, s)| s.whisper == whisper && Some(**id) != me).map(|(_, s)| s).collect();
        group.sort_by(|a, b| b.talking.cmp(&a.talking).then(a.since.elapsed().cmp(&b.since.elapsed())));
        if group.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(Row::Gap(cfg.int("talk_gap").max(0)));
        }
        let dot = cfg.color(if whisper { "color_whisper" } else { "color_talk" });
        out.extend(group.into_iter().map(|s| Row::Item {
            text: s.name.clone(),
            dot: if s.talking { dot } else { blend(dot, bg, 0.5) },
            color: if s.talking { text } else { faded },
        }));
    }
    out
}

/// Row height and padding derived from the font size.
fn metrics(font_px: i32) -> (i32, i32) {
    (font_px * 17 / 10, font_px / 2)
}

fn rows_height(rows: &[Row], row_h: i32) -> i32 {
    rows.iter().map(|r| if let Row::Gap(g) = r { *g } else { row_h }).sum()
}

/// Work areas of all monitors, left to right.
fn monitors() -> Vec<RECT> {
    unsafe extern "system" fn cb(m: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> i32 {
        unsafe {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(m, &mut mi) != 0 {
                (*(lp as *mut Vec<RECT>)).push(mi.rcWork);
            }
        }
        1
    }
    let mut v: Vec<RECT> = Vec::new();
    unsafe { EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(cb), &mut v as *mut _ as LPARAM) };
    v.sort_by_key(|r| r.left);
    v
}

/// Monitor names for the settings window; index matches the `*_monitor` value.
pub fn monitor_names() -> Vec<String> {
    let mut names = vec!["Самый правый".to_string()];
    names.extend(monitors().iter().enumerate().map(|(i, r)| format!("{} ({}x{})", i + 1, r.right - r.left, r.bottom - r.top)));
    names
}

fn layout_all() {
    layout(0);
    layout(1);
}

/// Show, hide, resize and position one overlay to fit its rows.
fn layout(i: usize) {
    let Some((hwnd, rect)) = UI.with_borrow(|ui| {
        let ui = ui.as_ref()?;
        let cfg = &ui.cfg;
        let p = PREFIX[i];
        let n = rows(i, cfg, &lock(&STATE));
        if ui.hidden[i] || !cfg.bool(&format!("{p}_enabled")) || n.is_empty() {
            return Some((ui.wins[i], None));
        }
        let (row_h, pad) = metrics(cfg.int(&format!("{p}_font_px")).max(6));
        let (w, h) = (cfg.int(&format!("{p}_width")).max(50), pad * 2 + rows_height(&n, row_h));
        let mons = monitors();
        let m = match cfg.int(&format!("{p}_monitor")) {
            k if k >= 1 && (k as usize) <= mons.len() => mons[k as usize - 1],
            _ => *mons.last()?,
        };
        let (ox, oy) = (cfg.int(&format!("{p}_x")), cfg.int(&format!("{p}_y")));
        let corner = cfg.int(&format!("{p}_corner"));
        let x = if corner % 2 == 0 { m.left + ox } else { m.right - w - ox };
        let y = if corner < 2 { m.top + oy } else { m.bottom - h - oy };
        Some((ui.wins[i], Some((x, y, w, h))))
    }) else { return };
    unsafe {
        match rect {
            None => {
                ShowWindow(hwnd, SW_HIDE);
            }
            Some((x, y, w, h)) => {
                // re-asserted every time: something else may have taken the topmost spot
                SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
        }
    }
}

/// Anti-aliased filled circle; GDI's Ellipse has jagged edges.
unsafe fn dot(hdc: HDC, x: i32, y: i32, d: i32, color: u32, bg: u32) {
    let r = d as f32 / 2.0;
    for py in 0..d {
        for px in 0..d {
            let (dx, dy) = (px as f32 + 0.5 - r, py as f32 + 0.5 - r);
            let cov = (r - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            if cov > 0.0 {
                unsafe { SetPixelV(hdc, x + px, y + py, blend(color, bg, cov)) };
            }
        }
    }
}

unsafe fn paint(hwnd: HWND, i: usize) {
    UI.with_borrow(|ui| {
        let Some(ui) = ui.as_ref() else { return };
        let cfg = &ui.cfg;
        let n = rows(i, cfg, &lock(&STATE));
        let (row_h, pad) = metrics(cfg.int(&format!("{}_font_px", PREFIX[i])).max(6));
        let d = (pad).max(4);
        let bg = cfg.color("color_bg");
        unsafe {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);
            let (w, h) = (rc.right, rc.bottom);
            let mem = CreateCompatibleDC(hdc);
            let bmp = CreateCompatibleBitmap(hdc, w, h);
            let old_bmp = SelectObject(mem, bmp);
            let old_font = SelectObject(mem, ui.fonts[i]);

            let brush = CreateSolidBrush(bg);
            FillRect(mem, &rc, brush);
            DeleteObject(brush);
            let brush = CreateSolidBrush(cfg.color("color_border"));
            FrameRect(mem, &rc, brush);
            DeleteObject(brush);
            SetBkMode(mem, TRANSPARENT as i32);

            let mut y = pad;
            for row in &n {
                match row {
                    Row::Gap(g) => y += g,
                    Row::Item { text, dot: c, color } => {
                        dot(mem, pad, y + (row_h - d) / 2, d, *c, bg);
                        SetTextColor(mem, *color);
                        let mut t: Vec<u16> = text.encode_utf16().collect();
                        let mut r = RECT { left: pad * 2 + d, top: y, right: w - pad, bottom: y + row_h };
                        DrawTextW(mem, t.as_mut_ptr(), t.len() as i32, &mut r, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
                        y += row_h;
                    }
                }
            }

            BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
            SelectObject(mem, old_font);
            SelectObject(mem, old_bmp);
            DeleteObject(bmp);
            DeleteDC(mem);
            EndPaint(hwnd, &ps);
        }
    });
}

/// Drops lingering speakers whose time is up.
fn expire() {
    let linger = Duration::from_secs(UI.with_borrow(|ui| ui.as_ref().map_or(15, |u| u.cfg.int("linger").max(0) as u64)));
    let changed = {
        let mut s = lock(&STATE);
        let before = s.speakers.len();
        s.speakers.retain(|_, sp| sp.talking || sp.since.elapsed() < linger);
        s.speakers.len() != before
    };
    if changed {
        layout_all();
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let kind = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        match (kind, msg) {
            (1 | 2, WM_PAINT) => paint(hwnd, kind as usize - 1),
            (1 | 2, WM_ERASEBKGND) => return 1,
            (KIND_CONTROL, WM_APP_REFRESH) => layout_all(),
            (KIND_CONTROL, WM_APP_RELOAD) => apply_config(hwnd),
            (KIND_CONTROL, WM_TIMER) => expire(),
            (KIND_CONTROL, WM_HOTKEY) if wp == 1 || wp == 2 => {
                UI.with_borrow_mut(|ui| {
                    if let Some(ui) = ui.as_mut() {
                        ui.hidden[wp - 1] ^= true;
                    }
                });
                layout(wp - 1);
            }
            (KIND_CONTROL, WM_CLOSE) => PostQuitMessage(0),
            _ => return DefWindowProcW(hwnd, msg, wp, lp),
        }
        0
    }
}
