//! The two overlay windows, owned by one UI thread.
//!
//! TS callbacks only update `STATE` and post a message here; all window work happens on this
//! thread. A message-only "control" window receives those posts, hotkeys and the linger timer.

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleHandleExW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, TME_LEAVE, TRACKMOUSEEVENT,
    ReleaseCapture, SetCapture, TrackMouseEvent, UnregisterHotKey,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::config::{self, Config};
use crate::{STATE, State, Tab, lock, log};

const WM_APP_REFRESH: u32 = WM_APP + 1;
const WM_APP_RELOAD: u32 = WM_APP + 2;
/// windows-sys keeps this one behind the Win32_UI_Controls feature.
const WM_MOUSELEAVE: u32 = 0x02A3;
const CLASS: &str = "TsTalkOverlay";
/// GWLP_USERDATA tags: index into `Ui::wins` + 1, or the control window.
const KIND_CONTROL: isize = 10;
const PREFIX: [&str; 2] = ["talk", "list"];
/// Hotkey id of the volume mode; 1 and 2 toggle the windows.
const HOTKEY_VOLUME: i32 = 3;
const HOTKEY_ARRANGE: i32 = 4;
/// Arrange mode: how close to a window's edge a press resizes instead of moving it.
const EDGE: i32 = 10;
const LEFT: u8 = 1;
const RIGHT: u8 = 2;
const TOP: u8 = 4;
const BOTTOM: u8 = 8;
/// The volume mode switches itself off after this long without a wheel or click.
const VOLUME_IDLE: Duration = Duration::from_secs(10);

static CONTROL: AtomicUsize = AtomicUsize::new(0);
static THREAD: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

struct Ui {
    wins: [HWND; 2],
    fonts: [HFONT; 2],
    /// Toggled by the hotkeys; not saved.
    hidden: [bool; 2],
    /// Volume mode: "Кто говорит" takes the mouse; time of the last action while it is on.
    volume_mode: Option<Instant>,
    /// "Кто говорит" rows as they were when the mouse came over it in volume mode. While the
    /// mouse stays there the list keeps this order and size, so the wheel never lands on
    /// someone else because a new speaker re-sorted it.
    frozen: Option<Vec<Frozen>>,
    /// Arrange mode: both windows take the mouse and are moved and resized with it.
    arrange: bool,
    drag: Option<Drag>,
    cfg: Config,
}

/// A press in arrange mode: which window, which edges it grabbed (none = move), where the
/// cursor and the window were then.
struct Drag {
    win: usize,
    edges: u8,
    start: POINT,
    rect: RECT,
    font: i32,
}

/// A speaker: (server tab, clid).
type Id = (u64, u16);

enum Frozen {
    Speaker { id: Id, whisper: bool, name: String },
    Header(String),
    Gap(i32),
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

enum Row {
    /// `id` is the speaker under this row; `volume` is shown on the right in volume mode.
    Item { text: String, dot: u32, color: u32, id: Option<Id>, volume: Option<i32> },
    /// Server tab heading, when more than one tab is connected.
    Header(String),
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
        UI.set(Some(Ui { wins, fonts: [std::ptr::null_mut(); 2], hidden: [false; 2], volume_mode: None, frozen: None, arrange: false, drag: None, cfg: config::current() }));
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
        UnregisterHotKey(ctrl, HOTKEY_VOLUME);
        UnregisterHotKey(ctrl, HOTKEY_ARRANGE);
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
            }
            ui.fonts[i] = make_font(&cfg, i);
        }
        ui.cfg = cfg.clone();
        ui.wins
    });
    let alpha = (cfg.int("opacity").clamp(10, 100) * 255 / 100) as u8;
    for (i, w) in wins.into_iter().enumerate() {
        unsafe { SetLayeredWindowAttributes(w, 0, alpha, LWA_ALPHA) };
        register_hotkey(ctrl, i as i32 + 1, cfg.get(&format!("{}_hotkey", PREFIX[i])));
    }
    register_hotkey(ctrl, HOTKEY_VOLUME, cfg.get("volume_hotkey"));
    register_hotkey(ctrl, HOTKEY_ARRANGE, cfg.get("arrange_hotkey"));
    layout_all();
}

fn make_font(cfg: &Config, i: usize) -> HFONT {
    let px = cfg.int(&format!("{}_font_px", PREFIX[i])).max(6);
    unsafe {
        CreateFontW(
            -px, 0, 0, 0, if cfg.bool("bold") { 700 } else { 400 }, 0, 0, 0,
            DEFAULT_CHARSET as u32, OUT_DEFAULT_PRECIS as u32, CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32, DEFAULT_PITCH as u32,
            wide(cfg.get("font")).as_ptr(),
        )
    }
}

fn register_hotkey(ctrl: HWND, id: i32, key: &str) {
    unsafe {
        UnregisterHotKey(ctrl, id);
        if let Some((mods, vk)) = parse_hotkey(key) {
            if RegisterHotKey(ctrl, id, mods | MOD_NOREPEAT, vk) == 0 {
                log(&format!("горячая клавиша {key} занята другой программой"));
            }
        } else if !key.trim().is_empty() {
            log(&format!("не понял горячую клавишу '{key}'"));
        }
    }
}

fn volume_mode() -> bool {
    UI.with_borrow(|ui| ui.as_ref().is_some_and(|u| u.volume_mode.is_some()))
}

/// Volume mode on: "Кто говорит" stops letting clicks through and shows each speaker's volume.
fn set_volume_mode(on: bool) {
    if on && arranging() {
        set_arrange(false);
    }
    let Some(hwnd) = UI.with_borrow_mut(|ui| {
        let ui = ui.as_mut()?;
        ui.volume_mode = on.then(Instant::now);
        ui.frozen = None;
        Some(ui.wins[0])
    }) else { return };
    click_through(hwnd, !on);
    layout(0);
}

fn click_through(hwnd: HWND, on: bool) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let ex = if on { ex | WS_EX_TRANSPARENT as isize } else { ex & !(WS_EX_TRANSPARENT as isize) };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
    }
}

fn arranging() -> bool {
    UI.with_borrow(|ui| ui.as_ref().is_some_and(|u| u.arrange))
}

/// Arrange mode on: both windows show up (with sample rows if empty) and take the mouse.
fn set_arrange(on: bool) {
    if on {
        set_volume_mode(false);
    }
    let Some(wins) = UI.with_borrow_mut(|ui| {
        let ui = ui.as_mut()?;
        ui.arrange = on;
        ui.drag = None;
        Some(ui.wins)
    }) else { return };
    if !on {
        unsafe { ReleaseCapture() };
    }
    for w in wins {
        click_through(w, !on);
    }
    layout_all();
}

/// Which edges of window `hwnd` are near client point (x, y); 0 = the middle.
fn edges_at(hwnd: HWND, x: i32, y: i32) -> u8 {
    let mut rc: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetClientRect(hwnd, &mut rc) };
    let mut e = 0;
    if x < EDGE {
        e |= LEFT;
    } else if x >= rc.right - EDGE {
        e |= RIGHT;
    }
    if y < EDGE {
        e |= TOP;
    } else if y >= rc.bottom - EDGE {
        e |= BOTTOM;
    }
    e
}

fn edge_cursor(e: u8) -> windows_sys::core::PCWSTR {
    match e {
        0 => IDC_SIZEALL,
        LEFT | RIGHT => IDC_SIZEWE,
        TOP | BOTTOM => IDC_SIZENS,
        e if e == LEFT | TOP || e == RIGHT | BOTTOM => IDC_SIZENWSE,
        _ => IDC_SIZENESW,
    }
}

fn start_drag(hwnd: HWND, i: usize, x: i32, y: i32) {
    let edges = edges_at(hwnd, x, y);
    let mut start = POINT { x: 0, y: 0 };
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        GetCursorPos(&mut start);
        GetWindowRect(hwnd, &mut rect);
        SetCapture(hwnd);
    }
    UI.with_borrow_mut(|ui| {
        if let Some(ui) = ui.as_mut().filter(|u| u.arrange) {
            let font = ui.cfg.int(&format!("{}_font_px", PREFIX[i])).max(6);
            ui.drag = Some(Drag { win: i, edges, start, rect, font });
        }
    });
}

/// Follows the cursor during a drag: moves the window or resizes it. The sides change the
/// width; the top and bottom change the font size, and the height follows the rows. The result
/// goes straight into the window's settings as monitor, corner and offsets.
fn drag_move() {
    let mut pt = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut pt) };
    let Some(i) = UI.with_borrow_mut(|ui| {
        let ui = ui.as_mut()?;
        let d = ui.drag.as_ref()?;
        let (i, r, p) = (d.win, d.rect, PREFIX[d.win]);
        let (dx, dy) = (pt.x - d.start.x, pt.y - d.start.y);
        let (mut x, mut y, mut w) = (r.left, r.top, r.right - r.left);
        if d.edges == 0 {
            x += dx;
            y += dy;
        }
        if d.edges & LEFT != 0 {
            w = (w - dx).max(80);
            x = r.right - w;
        }
        if d.edges & RIGHT != 0 {
            w = (w + dx).max(80);
        }
        let mut font = d.font;
        if d.edges & (TOP | BOTTOM) != 0 {
            let h0 = (r.bottom - r.top).max(1);
            let dh = if d.edges & TOP != 0 { -dy } else { dy };
            font = (d.font * (h0 + dh) / h0).clamp(8, 96);
        }
        let (edges, top, bottom) = (d.edges, r.top, r.bottom);
        if font != ui.cfg.int(&format!("{p}_font_px")) {
            set_key(&mut ui.cfg, &format!("{p}_font_px"), font);
            unsafe { DeleteObject(ui.fonts[i]) };
            ui.fonts[i] = make_font(&ui.cfg, i);
        }
        let (row_h, pad) = metrics(font);
        let h = pad * 2 + rows_height(&shown_rows(ui, i), row_h);
        if edges & TOP != 0 {
            y = bottom - h;
        } else if edges != 0 {
            y = top;
        }
        store_position(&mut ui.cfg, i, x, y, w, h);
        Some(i)
    }) else { return };
    layout(i);
}

/// End of a drag: the new position and size go into the ini.
fn end_drag() {
    let Some(i) = UI.with_borrow_mut(|ui| ui.as_mut()?.drag.take().map(|d| d.win)) else { return };
    unsafe { ReleaseCapture() };
    let p = PREFIX[i];
    let keys = ["monitor", "corner", "x", "y", "width", "font_px"].map(|k| format!("{p}_{k}"));
    UI.with_borrow(|ui| {
        let Some(ui) = ui.as_ref() else { return };
        let mut cfg = lock(&config::CONFIG);
        for k in &keys {
            set_key(&mut cfg, k, ui.cfg.int(k));
        }
        cfg.save();
    });
}

fn set_key(cfg: &mut Config, key: &str, v: i32) {
    if let Some(f) = config::fields().find(|f| f.key == key) {
        cfg.set(f.key, v.to_string());
    }
}

/// Screen rect -> the monitor it is on, the nearest corner of that monitor and offsets from it.
fn store_position(cfg: &mut Config, i: usize, x: i32, y: i32, w: i32, h: i32) {
    let p = PREFIX[i];
    let mons = monitors();
    let (cx, cy) = (x + w / 2, y + h / 2);
    let Some(k) = mons
        .iter()
        .position(|m| (m.left..m.right).contains(&cx) && (m.top..m.bottom).contains(&cy))
        .or_else(|| mons.iter().position(|m| (m.left..m.right).contains(&cx)))
        .or_else(|| (!mons.is_empty()).then(|| if cx < mons[0].left { 0 } else { mons.len() - 1 }))
    else { return };
    let m = mons[k];
    let old = cfg.int(&format!("{p}_monitor"));
    // "Самый правый" stays as it is while the window stays on the rightmost monitor
    let monitor = if k == mons.len() - 1 && !(1..=mons.len() as i32).contains(&old) { old } else { k as i32 + 1 };
    let (left, top) = (cx < (m.left + m.right) / 2, cy < (m.top + m.bottom) / 2);
    set_key(cfg, &format!("{p}_monitor"), monitor);
    set_key(cfg, &format!("{p}_corner"), !left as i32 + 2 * !top as i32);
    set_key(cfg, &format!("{p}_x"), if left { x - m.left } else { m.right - x - w });
    set_key(cfg, &format!("{p}_y"), if top { y - m.top } else { m.bottom - y - h });
    set_key(cfg, &format!("{p}_width"), w);
}

/// Speaker on the "Кто говорит" row at client y, with their current volume.
fn speaker_at(y: i32) -> Option<(Id, i32)> {
    UI.with_borrow(|ui| {
        let ui = ui.as_ref()?;
        let cfg = &ui.cfg;
        let n = rows(0, cfg, &lock(&STATE), true, ui.frozen.as_deref());
        let (row_h, pad) = metrics(cfg.int("talk_font_px").max(6));
        let mut top = pad;
        for row in &n {
            match row {
                Row::Gap(g) => top += g,
                Row::Header(_) => top += row_h,
                Row::Item { id, volume, .. } => {
                    if (top..top + row_h).contains(&y) {
                        return Some(((*id)?, volume.unwrap_or(0)));
                    }
                    top += row_h;
                }
            }
        }
        None
    })
}

/// Wheel notches (positive = louder) or a reset to 0 dB for the speaker at client y.
fn adjust_volume(y: i32, notches: Option<i32>) {
    UI.with_borrow_mut(|ui| {
        if let Some(ui) = ui.as_mut().filter(|u| u.volume_mode.is_some()) {
            ui.volume_mode = Some(Instant::now());
        }
    });
    if let Some((id, current)) = speaker_at(y) {
        crate::set_volume(id.0, id.1, notches.map_or(0, |n| current + n));
    }
}

/// Mouse over "Кто говорит" in volume mode: freeze the rows until it leaves.
fn freeze(hwnd: HWND) {
    let froze = UI.with_borrow_mut(|ui| {
        let ui = ui.as_mut().filter(|u| u.volume_mode.is_some() && u.frozen.is_none())?;
        let st = lock(&STATE);
        let frozen: Vec<Frozen> = rows(0, &ui.cfg, &st, true, None)
            .into_iter()
            .filter_map(|r| match r {
                Row::Gap(g) => Some(Frozen::Gap(g)),
                Row::Header(h) => Some(Frozen::Header(h)),
                Row::Item { id: Some(id), text, .. } => {
                    let sp = st.tabs.get(&id.0).and_then(|t| t.speakers.get(&id.1));
                    Some(Frozen::Speaker { id, whisper: sp.is_some_and(|s| s.whisper), name: text })
                }
                Row::Item { id: None, .. } => None,
            })
            .collect();
        // "никто не говорит": nothing to hold still, and freezing would hide the first speaker
        if !frozen.iter().any(|f| matches!(f, Frozen::Speaker { .. })) {
            return None;
        }
        ui.frozen = Some(frozen);
        Some(())
    });
    if froze.is_some() {
        let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
        unsafe { TrackMouseEvent(&mut tme) };
    }
}

fn unfreeze() {
    UI.with_borrow_mut(|ui| {
        if let Some(ui) = ui.as_mut() {
            ui.frozen = None;
        }
    });
    layout(0);
}

fn blend(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| {
        let (x, y) = ((a >> s & 0xff) as f32, (b >> s & 0xff) as f32);
        ((x * t + y * (1.0 - t)).round() as u32) << s
    };
    ch(0) | ch(8) | ch(16)
}

fn rows(i: usize, cfg: &Config, st: &State, volume_mode: bool, frozen: Option<&[Frozen]>) -> Vec<Row> {
    let (text, faded, bg) = (cfg.color("color_text"), cfg.color("color_faded"), cfg.color("color_bg"));
    let speaker = |id: &Id| st.tabs.get(&id.0).and_then(|t| t.speakers.get(&id.1));
    if let (0, Some(frozen)) = (i, frozen) {
        // same rows in the same places; only talking state and volume are live
        return frozen
            .iter()
            .map(|f| match f {
                Frozen::Gap(g) => Row::Gap(*g),
                Frozen::Header(h) => Row::Header(h.clone()),
                Frozen::Speaker { id, whisper, name } => {
                    let dot = cfg.color(voice_color(*whisper, id.0 != st.active));
                    match speaker(id) {
                        Some(s) => Row::Item {
                            text: s.name.clone(),
                            dot: if s.talking { dot } else { blend(dot, bg, 0.5) },
                            color: if s.talking { text } else { faded },
                            id: Some(*id),
                            volume: Some(s.volume),
                        },
                        // left the server meanwhile: keep the row, but the wheel does nothing on it
                        None => Row::Item { text: name.clone(), dot: blend(dot, bg, 0.2), color: faded, id: None, volume: None },
                    }
                }
            })
            .collect();
    }
    // One block per tab, the active tab first; headings only when more than one is connected.
    let headings = st.tabs.len() > 1;
    let tab_gap = if i == 0 { cfg.int("talk_gap").max(0) } else { cfg.int("list_font_px").max(6) / 2 };
    let mut out = Vec::new();
    for (sch, tab) in st.ordered() {
        let me = (!cfg.bool("show_self")).then_some(tab.self_id);
        let other = sch != st.active;
        let block = if i == 1 { list_block(cfg, tab, me, other) } else { talk_block(cfg, sch, tab, me, other, volume_mode) };
        if block.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(Row::Gap(tab_gap));
        }
        if headings {
            out.push(Row::Header(tab.name.clone()));
        }
        out.extend(block);
    }
    if i == 0 && volume_mode && out.is_empty() {
        let hint = "Громкость: никто не говорит".to_string();
        out.push(Row::Item { text: hint, dot: bg, color: faded, id: None, volume: None });
    }
    out
}

/// Dot colour of a speaker: whisper, channel voice on the active tab, or channel voice on another
/// tab (`other`).
fn voice_color(whisper: bool, other: bool) -> &'static str {
    match (whisper, other) {
        (true, _) => "color_whisper",
        (false, false) => "color_talk",
        (false, true) => "color_other_tab",
    }
}

/// "Список канала" rows of one tab: everyone in our channel, by name.
fn list_block(cfg: &Config, tab: &Tab, me: Option<u16>, other: bool) -> Vec<Row> {
    let (text, faded, bg) = (cfg.color("color_text"), cfg.color("color_faded"), cfg.color("color_bg"));
    let mut people: Vec<_> = tab.channel.iter().filter(|(id, _)| Some(*id) != me).collect();
    people.sort_by_key(|(_, n)| n.to_lowercase());
    people
        .into_iter()
        .map(|(id, name)| {
            let talking = tab.speakers.get(id).is_some_and(|s| s.talking && !s.whisper);
            let dot = if talking { cfg.color(voice_color(false, other)) } else { blend(faded, bg, 0.35) };
            let color = if tab.joined.contains_key(id) { cfg.color("color_join") } else { text };
            Row::Item { text: name.clone(), dot, color, id: None, volume: None }
        })
        .collect()
}

/// "Кто говорит" rows of one tab: whispers first, then a gap, then channel voice; within each
/// group live speakers above lingering ones, newest first.
fn talk_block(cfg: &Config, sch: u64, tab: &Tab, me: Option<u16>, other: bool, volume_mode: bool) -> Vec<Row> {
    let (text, faded, bg) = (cfg.color("color_text"), cfg.color("color_faded"), cfg.color("color_bg"));
    let mut out = Vec::new();
    for whisper in [true, false] {
        let mut group: Vec<_> = tab.speakers.iter().filter(|(id, s)| s.whisper == whisper && Some(**id) != me).collect();
        group.sort_by(|(_, a), (_, b)| b.talking.cmp(&a.talking).then(a.since.elapsed().cmp(&b.since.elapsed())));
        if group.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(Row::Gap(cfg.int("talk_gap").max(0)));
        }
        let dot = cfg.color(voice_color(whisper, other));
        out.extend(group.into_iter().map(|(id, s)| Row::Item {
            text: s.name.clone(),
            dot: if s.talking { dot } else { blend(dot, bg, 0.5) },
            color: if s.talking { text } else { faded },
            id: Some((sch, *id)),
            volume: volume_mode.then_some(s.volume),
        }));
    }
    out
}

/// The rows a window shows; in arrange mode an empty window gets sample rows, so it can be seen
/// and grabbed.
fn shown_rows(ui: &Ui, i: usize) -> Vec<Row> {
    let cfg = &ui.cfg;
    let n = rows(i, cfg, &lock(&STATE), i == 0 && ui.volume_mode.is_some(), ui.frozen.as_deref());
    if !ui.arrange || !n.is_empty() {
        return n;
    }
    let item = |text: &str, dot: &str| Row::Item { text: text.to_string(), dot: cfg.color(dot), color: cfg.color("color_text"), id: None, volume: None };
    if i == 0 {
        vec![item("Шёпот", "color_whisper"), Row::Gap(cfg.int("talk_gap").max(0)), item("Кто говорит", "color_talk")]
    } else {
        vec![item("Список канала", "color_talk")]
    }
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
        let n = shown_rows(ui, i);
        if (ui.hidden[i] && !ui.arrange) || !cfg.bool(&format!("{p}_enabled")) || n.is_empty() {
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
        let volume_mode = i == 0 && ui.volume_mode.is_some();
        let n = shown_rows(ui, i);
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
            // volume / arrange mode: a thick frame, so it is obvious the window takes the mouse
            let frame = if ui.arrange { "color_join" } else if volume_mode { "color_talk" } else { "color_border" };
            let brush = CreateSolidBrush(cfg.color(frame));
            for k in 0..if volume_mode || ui.arrange { 3 } else { 1 } {
                FrameRect(mem, &RECT { left: k, top: k, right: w - k, bottom: h - k }, brush);
            }
            DeleteObject(brush);
            SetBkMode(mem, TRANSPARENT as i32);

            let mut y = pad;
            for row in &n {
                match row {
                    Row::Gap(g) => y += g,
                    Row::Header(h) => {
                        // the tab heading: faded, no dot, from the left edge
                        SetTextColor(mem, cfg.color("color_faded"));
                        let mut t: Vec<u16> = h.encode_utf16().collect();
                        let mut r = RECT { left: pad, top: y, right: w - pad, bottom: y + row_h };
                        DrawTextW(mem, t.as_mut_ptr(), t.len() as i32, &mut r, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
                        y += row_h;
                    }
                    Row::Item { text, dot: c, color, volume, .. } => {
                        dot(mem, pad, y + (row_h - d) / 2, d, *c, bg);
                        let mut right = w - pad;
                        if let Some(v) = volume {
                            let mut t: Vec<u16> = format!("{v:+} дБ").replace("+0", "0").encode_utf16().collect();
                            let mut size = SIZE { cx: 0, cy: 0 };
                            GetTextExtentPoint32W(mem, t.as_ptr(), t.len() as i32, &mut size);
                            SetTextColor(mem, cfg.color(if *v == 0 { "color_faded" } else { "color_whisper" }));
                            let mut r = RECT { left: right - size.cx, top: y, right, bottom: y + row_h };
                            DrawTextW(mem, t.as_mut_ptr(), t.len() as i32, &mut r, DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
                            right -= size.cx + pad;
                        }
                        SetTextColor(mem, *color);
                        let mut t: Vec<u16> = text.encode_utf16().collect();
                        let mut r = RECT { left: pad * 2 + d, top: y, right, bottom: y + row_h };
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

/// Drops lingering speakers and join highlights whose time is up, and ends an idle volume mode.
/// While the volume mode is on nobody is dropped, so the row under the mouse stays put.
fn expire() {
    let idle = UI.with_borrow(|ui| ui.as_ref().and_then(|u| u.volume_mode).map(|t| t.elapsed()));
    if idle.is_some_and(|t| t >= VOLUME_IDLE) {
        set_volume_mode(false);
    }
    let volume_mode = volume_mode();
    let secs = |key: &str| Duration::from_secs(UI.with_borrow(|ui| ui.as_ref().map_or(15, |u| u.cfg.int(key).max(0) as u64)));
    let (linger, join) = (secs("linger"), secs("list_join_secs"));
    let changed = {
        let mut s = lock(&STATE);
        let mut changed = false;
        for tab in s.tabs.values_mut() {
            let before = (tab.speakers.len(), tab.joined.len());
            tab.speakers.retain(|_, sp| volume_mode || sp.talking || sp.since.elapsed() < linger);
            tab.joined.retain(|_, t| t.elapsed() < join);
            changed |= (tab.speakers.len(), tab.joined.len()) != before;
        }
        changed
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
            // the window only gets the mouse in volume mode; it must never take focus from the game
            (1 | 2, WM_MOUSEACTIVATE) => return MA_NOACTIVATE as LRESULT,
            (1 | 2, WM_SETCURSOR) if arranging() => {
                let mut pt = POINT { x: 0, y: 0 };
                GetCursorPos(&mut pt);
                ScreenToClient(hwnd, &mut pt);
                SetCursor(LoadCursorW(std::ptr::null_mut(), edge_cursor(edges_at(hwnd, pt.x, pt.y))));
                return 1;
            }
            (1 | 2, WM_LBUTTONDOWN) if arranging() => {
                start_drag(hwnd, kind as usize - 1, (lp & 0xffff) as i16 as i32, (lp >> 16 & 0xffff) as i16 as i32)
            }
            (1 | 2, WM_LBUTTONUP | WM_CAPTURECHANGED) => end_drag(),
            (1 | 2, WM_MOUSEMOVE) if arranging() => drag_move(),
            (1, WM_MOUSEWHEEL) if volume_mode() => {
                let mut pt = POINT { x: (lp & 0xffff) as i16 as i32, y: (lp >> 16 & 0xffff) as i16 as i32 };
                ScreenToClient(hwnd, &mut pt);
                let delta = (wp >> 16 & 0xffff) as i16 as i32;
                adjust_volume(pt.y, Some(delta.signum() * (delta.abs() / 120).max(1)));
            }
            (1, WM_RBUTTONUP) if volume_mode() => adjust_volume((lp >> 16 & 0xffff) as i16 as i32, None),
            (1, WM_MOUSEMOVE) => freeze(hwnd),
            (1, WM_MOUSELEAVE) => unfreeze(),
            (KIND_CONTROL, WM_HOTKEY) if wp == HOTKEY_VOLUME as usize => set_volume_mode(!volume_mode()),
            (KIND_CONTROL, WM_HOTKEY) if wp == HOTKEY_ARRANGE as usize => set_arrange(!arranging()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Speaker;

    fn tab(name: &str, speakers: &[(u16, &str, bool)], channel: &[(u16, &str)]) -> Tab {
        let mut t = Tab { name: name.to_string(), self_id: 99, ..Default::default() };
        for &(id, n, whisper) in speakers {
            t.speakers.insert(id, Speaker { name: n.to_string(), talking: true, whisper, since: Instant::now(), volume: 0 });
        }
        t.channel = channel.iter().map(|&(id, n)| (id, n.to_string())).collect();
        t
    }

    /// Text of each row: "#heading", "name", or "-" for a gap.
    fn texts(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Header(h) => format!("#{h}"),
                Row::Item { text, .. } => text.clone(),
                Row::Gap(_) => "-".to_string(),
            })
            .collect()
    }

    #[test]
    fn tabs_are_grouped_under_headings_active_first() {
        let cfg = config::current();
        let mut st = State { active: 2, ..Default::default() };
        st.tabs.insert(1, tab("Первый", &[(5, "Ворон", true), (6, "Grom", false)], &[(5, "Ворон"), (7, "Тихоня")]));
        st.tabs.insert(2, tab("Второй", &[(5, "Лиса", false)], &[(8, "Мельник")]));
        assert_eq!(texts(&rows(0, &cfg, &st, false, None)), ["#Второй", "Лиса", "-", "#Первый", "Ворон", "-", "Grom"]);
        assert_eq!(texts(&rows(1, &cfg, &st, false, None)), ["#Второй", "Мельник", "-", "#Первый", "Ворон", "Тихоня"]);
        // the same clid on two tabs is two different people
        let ids: Vec<_> = rows(0, &cfg, &st, false, None).iter().filter_map(|r| if let Row::Item { id, .. } = r { *id } else { None }).collect();
        assert_eq!(ids, [(2, 5), (1, 5), (1, 6)]);
        // channel voice: green on the active tab, its own colour on another; a whisper stays a whisper
        let dots: Vec<_> = rows(0, &cfg, &st, false, None).iter().filter_map(|r| if let Row::Item { dot, .. } = r { Some(*dot) } else { None }).collect();
        assert_eq!(dots, [cfg.color("color_talk"), cfg.color("color_whisper"), cfg.color("color_other_tab")]);

        // one tab: no heading, looks as before
        st.tabs.remove(&2);
        assert_eq!(texts(&rows(0, &cfg, &st, false, None)), ["Ворон", "-", "Grom"]);
        // a tab with nobody talking gets no block at all
        st.tabs.insert(3, tab("Третий", &[], &[]));
        assert_eq!(texts(&rows(0, &cfg, &st, false, None)), ["#Первый", "Ворон", "-", "Grom"]);
    }
}
