//! Settings window, shown from `ts3plugin_configure` on a thread TS creates for it.
//! Laid out from `config::SECTIONS`, one column per section.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{COLOR_BTNFACE, DEFAULT_GUI_FONT, GetStockObject, HBRUSH};
use windows_sys::Win32::UI::Controls::Dialogs::{CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW, ChooseColorW};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::config::{self, CONFIG, Field, Kind};
use crate::lock;
use crate::overlay::{self, module, wide};

const ID_FIRST: i32 = 1000;
const ID_APPLY: i32 = 3;
const COL_W: i32 = 290;
const ROW_H: i32 = 28;

static OPEN: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// One control per field, in `config::fields()` order.
    static CONTROLS: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
    static CUSTOM_COLORS: RefCell<[u32; 16]> = const { RefCell::new([0xffffff; 16]) };
}

fn text_of(h: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Reads the controls into the config, saves it and applies it to the overlays.
fn apply() {
    let controls = CONTROLS.with_borrow(|c| c.clone());
    let mut cfg = lock(&CONFIG);
    for (f, &h) in config::fields().zip(&controls) {
        let value = match f.kind {
            Kind::Text => text_of(h).trim().to_string(),
            Kind::Int => match text_of(h).trim().parse::<i32>() {
                Ok(v) => v.to_string(),
                Err(_) => continue, // keep the old value
            },
            Kind::Bool => (unsafe { SendMessageW(h, BM_GETCHECK, 0, 0) } == 1).then_some("1").unwrap_or("0").into(),
            Kind::Color => text_of(h),
            Kind::Choice(_) | Kind::Monitor => unsafe { SendMessageW(h, CB_GETCURSEL, 0, 0) }.max(0).to_string(),
        };
        cfg.set(f.key, value);
    }
    cfg.save();
    drop(cfg);
    overlay::reload();
}

fn pick_color(owner: HWND, button: HWND) {
    let Some(current) = config::parse_color(&text_of(button)) else { return };
    CUSTOM_COLORS.with_borrow_mut(|custom| unsafe {
        let mut cc: CHOOSECOLORW = std::mem::zeroed();
        cc.lStructSize = size_of::<CHOOSECOLORW>() as u32;
        cc.hwndOwner = owner;
        cc.rgbResult = current;
        cc.lpCustColors = custom.as_mut_ptr();
        cc.Flags = CC_RGBINIT | CC_FULLOPEN;
        if ChooseColorW(&mut cc) != 0 {
            SetWindowTextW(button, wide(&config::format_color(cc.rgbResult)).as_ptr());
        }
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let id = (wp & 0xffff) as i32;
                match id {
                    IDOK => {
                        apply();
                        DestroyWindow(hwnd);
                    }
                    ID_APPLY => apply(),
                    IDCANCEL => {
                        DestroyWindow(hwnd);
                    }
                    _ if id >= ID_FIRST => {
                        let i = (id - ID_FIRST) as usize;
                        if config::fields().nth(i).is_some_and(|f| f.kind == Kind::Color) {
                            pick_color(hwnd, lp as HWND);
                        }
                    }
                    _ => {}
                }
                0
            }
            WM_CLOSE => {
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

pub fn show() {
    if OPEN.swap(true, Ordering::AcqRel) {
        return; // already open
    }
    let cfg = config::current();
    let monitors = overlay::monitor_names();
    let class = wide("TsTalkOverlaySettings");
    let rows = config::SECTIONS.iter().map(|(_, f)| f.len() as i32).max().unwrap_or(0);
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: module(),
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_BTNFACE + 1) as usize as HBRUSH,
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);

        let (w, h) = (12 + COL_W * config::SECTIONS.len() as i32 + 16, 40 + ROW_H * (rows + 1) + 90);
        let x = (GetSystemMetrics(SM_CXSCREEN) - w) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - h) / 2;
        let hwnd = CreateWindowExW(
            WS_EX_DLGMODALFRAME | WS_EX_TOPMOST, class.as_ptr(), wide("TS Talk Overlay — настройки").as_ptr(),
            WS_CAPTION | WS_SYSMENU | WS_VISIBLE, x, y, w, h,
            std::ptr::null_mut(), std::ptr::null_mut(), module(), std::ptr::null(),
        );
        let font = GetStockObject(DEFAULT_GUI_FONT) as WPARAM;
        let child = |cls: &str, text: &str, style: u32, x, y, w, h, id: i32| {
            let c = CreateWindowExW(
                0, wide(cls).as_ptr(), wide(text).as_ptr(), WS_CHILD | WS_VISIBLE | style,
                x, y, w, h, hwnd, id as isize as _, module(), std::ptr::null(),
            );
            SendMessageW(c, WM_SETFONT, font, 1);
            c
        };

        let mut controls = Vec::new();
        let mut id = ID_FIRST;
        for (col, (title, fields)) in config::SECTIONS.iter().enumerate() {
            let x0 = 12 + COL_W * col as i32;
            child("BUTTON", title, BS_GROUPBOX as u32, x0, 8, COL_W - 10, ROW_H * (rows + 1) + 8, 0);
            for (row, f) in fields.iter().enumerate() {
                let y0 = 32 + ROW_H * row as i32;
                controls.push(field_control(&child, f, cfg.get(f.key), &monitors, x0 + 10, y0, id));
                id += 1;
            }
        }
        CONTROLS.set(controls);

        let by = 40 + ROW_H * (rows + 1);
        child("STATIC", "Горячая клавиша: например Ctrl+Alt+T или F9, пусто — без клавиши. \
                         Отступы считаются от выбранного угла рабочей области монитора.",
              0, 12, by - 20, w - 40, 36, 0);
        child("BUTTON", "OK", (BS_DEFPUSHBUTTON as u32) | WS_TABSTOP, w - 330, by + 20, 96, 28, IDOK);
        child("BUTTON", "Применить", WS_TABSTOP, w - 226, by + 20, 96, 28, ID_APPLY);
        child("BUTTON", "Отмена", WS_TABSTOP, w - 122, by + 20, 96, 28, IDCANCEL);
        SetForegroundWindow(hwnd);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        UnregisterClassW(class.as_ptr(), module());
    }
    OPEN.store(false, Ordering::Release);
}

fn field_control(
    child: &dyn Fn(&str, &str, u32, i32, i32, i32, i32, i32) -> HWND,
    f: &Field,
    value: &str,
    monitors: &[String],
    x: i32,
    y: i32,
    id: i32,
) -> HWND {
    const LABEL_W: i32 = 150;
    const CTRL_W: i32 = 116;
    unsafe {
        if f.kind == Kind::Bool {
            let c = child("BUTTON", f.label, (BS_AUTOCHECKBOX as u32) | WS_TABSTOP, x, y, LABEL_W + CTRL_W, 22, id);
            SendMessageW(c, BM_SETCHECK, if value == "1" { 1 } else { 0 }, 0);
            return c;
        }
        child("STATIC", f.label, 0, x, y + 3, LABEL_W, 20, 0);
        let cx = x + LABEL_W + 4;
        match f.kind {
            Kind::Color => child("BUTTON", value, WS_TABSTOP, cx, y, CTRL_W, 24, id),
            Kind::Choice(_) | Kind::Monitor => {
                let options: Vec<String> = match f.kind {
                    Kind::Choice(o) => o.iter().map(|s| s.to_string()).collect(),
                    _ => monitors.to_vec(),
                };
                let c = child("COMBOBOX", "", (CBS_DROPDOWNLIST as u32) | WS_VSCROLL | WS_TABSTOP, cx, y, CTRL_W, 200, id);
                for o in &options {
                    SendMessageW(c, CB_ADDSTRING, 0, wide(o).as_ptr() as LPARAM);
                }
                let sel = value.parse::<usize>().unwrap_or(0).min(options.len().saturating_sub(1));
                SendMessageW(c, CB_SETCURSEL, sel, 0);
                c
            }
            _ => child("EDIT", value, (WS_BORDER | WS_TABSTOP) | (ES_AUTOHSCROLL as u32), cx, y, CTRL_W, 22, id),
        }
    }
}
