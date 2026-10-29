//! TeamSpeak 3 plugin: two always-on-top click-through overlays.
//!
//! "Кто говорит" lists current speakers, whispers on top and channel voice below; a nick
//! lingers for a while after the person stops. "Список канала" lists everyone in our channel.
//! Works on the active server tab.
//! Settings live in `ts3_talk_overlay.ini` next to the DLL.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

mod config;
mod overlay;
mod settings;

pub const NAME: &str = "ts3_talk_overlay";
const STATUS_TALKING: c_int = 1;
const STATUS_DISCONNECTED: c_int = 0;
const STATUS_CONNECTION_ESTABLISHED: c_int = 4;
const LEAVE_VISIBILITY: c_int = 2;
const CLIENT_NICKNAME: usize = 1;
const CLIENT_VOLUME_MODIFICATOR: usize = 18;
/// Range of the per-client volume the overlay lets you set, dB.
pub const VOLUME_RANGE: (i32, i32) = (-40, 20);

/// The few TS3Functions we call, by index in `struct TS3Functions` (ts3_functions.h, API 26).
struct Ts3Fns {
    free_memory: unsafe extern "C" fn(*mut c_void) -> c_uint,
    set_client_volume_modifier: unsafe extern "C" fn(u64, u16, f32) -> c_uint,
    get_client_id: unsafe extern "C" fn(u64, *mut u16) -> c_uint,
    get_client_variable_as_int: unsafe extern "C" fn(u64, u16, usize, *mut c_int) -> c_uint,
    get_client_variable_as_string: unsafe extern "C" fn(u64, u16, usize, *mut *mut c_char) -> c_uint,
    get_channel_of_client: unsafe extern "C" fn(u64, u16, *mut u64) -> c_uint,
    get_channel_client_list: unsafe extern "C" fn(u64, u64, *mut *mut u16) -> c_uint,
    get_current_server_connection_handler_id: unsafe extern "C" fn() -> u64,
    get_server_connect_info: unsafe extern "C" fn(u64, *mut c_char, *mut u16, *mut c_char, usize) -> c_uint,
}

static TS3: OnceLock<Ts3Fns> = OnceLock::new();

pub struct Speaker {
    pub name: String,
    pub talking: bool,
    pub whisper: bool,
    /// When they started talking, or when they stopped if `talking` is false.
    pub since: Instant,
    /// Our local volume adjustment for them, dB.
    pub volume: i32,
}

#[derive(Default)]
pub struct State {
    /// Active server tab; events from other tabs are ignored.
    pub sch: u64,
    /// Host of the active tab, only to log server switches once.
    host: Option<String>,
    pub self_id: u16,
    channel_id: u64,
    pub speakers: HashMap<u16, Speaker>,
    /// Everyone in our channel: (clid, nickname).
    pub channel: Vec<(u16, String)>,
    /// When someone came into our channel, while they are highlighted.
    pub joined: HashMap<u16, Instant>,
}

pub static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);
pub static PLUGIN_DIR: LazyLock<PathBuf> = LazyLock::new(plugin_dir);

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn plugin_dir() -> PathBuf {
    let mut buf = [0u16; 1024];
    let len = unsafe {
        windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW(overlay::module(), buf.as_mut_ptr(), buf.len() as u32)
    };
    let dll = PathBuf::from(std::ffi::OsString::from_wide(&buf[..len as usize]));
    dll.parent().map(PathBuf::from).unwrap_or_default()
}

pub fn log(msg: &str) {
    let path = PLUGIN_DIR.join(format!("{NAME}.log"));
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{msg}");
    }
}

fn nickname(sch: u64, clid: u16) -> String {
    let Some(f) = TS3.get() else { return String::new() };
    let mut p: *mut c_char = std::ptr::null_mut();
    if unsafe { (f.get_client_variable_as_string)(sch, clid, CLIENT_NICKNAME, &mut p) } != 0 || p.is_null() {
        return format!("clid {clid}");
    }
    let name = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    unsafe { (f.free_memory)(p.cast()) };
    name
}

fn volume(sch: u64, clid: u16) -> i32 {
    let Some(f) = TS3.get() else { return 0 };
    let mut v: c_int = 0;
    if unsafe { (f.get_client_variable_as_int)(sch, clid, CLIENT_VOLUME_MODIFICATOR, &mut v) } != 0 {
        return 0;
    }
    v
}

/// Sets our local volume for a client on the active tab, clamped to `VOLUME_RANGE`.
/// Called from the overlay thread.
pub fn set_volume(clid: u16, db: i32) {
    let Some(f) = TS3.get() else { return };
    let db = db.clamp(VOLUME_RANGE.0, VOLUME_RANGE.1);
    let sch = lock(&STATE).sch;
    if sch == 0 {
        return;
    }
    let err = unsafe { (f.set_client_volume_modifier)(sch, clid, db as f32) };
    if err != 0 {
        return log(&format!("не удалось изменить громкость clid {clid}: ошибка {err}"));
    }
    if let Some(sp) = lock(&STATE).speakers.get_mut(&clid) {
        sp.volume = db;
    }
    overlay::refresh();
}

/// Our own clid, our channel and everyone in it, or None when not connected.
fn read_channel(sch: u64) -> Option<(u16, u64, Vec<(u16, String)>)> {
    let f = TS3.get()?;
    let (mut me, mut cid, mut list) = (0u16, 0u64, std::ptr::null_mut::<u16>());
    unsafe {
        if (f.get_client_id)(sch, &mut me) != 0
            || (f.get_channel_of_client)(sch, me, &mut cid) != 0
            || (f.get_channel_client_list)(sch, cid, &mut list) != 0
            || list.is_null()
        {
            return None;
        }
        let mut ids = Vec::new();
        let mut p = list;
        while *p != 0 {
            ids.push(*p);
            p = p.add(1);
        }
        (f.free_memory)(list.cast());
        Some((me, cid, ids.into_iter().map(|id| (id, nickname(sch, id))).collect()))
    }
}

/// Host the tab is connected to, or None when it is not connected.
fn server_host(sch: u64) -> Option<String> {
    let f = TS3.get()?;
    let (mut host, mut password, mut port) = ([0 as c_char; 256], [0 as c_char; 256], 0u16);
    let err = unsafe { (f.get_server_connect_info)(sch, host.as_mut_ptr(), &mut port, password.as_mut_ptr(), host.len()) };
    (err == 0).then(|| unsafe { CStr::from_ptr(host.as_ptr()) }.to_string_lossy().trim().to_string())
}

/// Re-reads the channel membership of the active tab. TS calls are made before taking the lock.
/// A tab that is not connected counts as no tab (sch 0), so nothing is shown.
fn refresh_channel() {
    let Some(f) = TS3.get() else { return };
    let current = unsafe { (f.get_current_server_connection_handler_id)() };
    let host = server_host(current);
    let sch = if host.is_some() { current } else { 0 };
    let (me, cid, channel) = if sch != 0 { read_channel(sch).unwrap_or_default() } else { Default::default() };
    {
        let mut s = lock(&STATE);
        if s.sch != sch {
            s.speakers.clear();
        }
        // Only people arriving in the channel we already sit in; not everyone on connect or when
        // we switch channels ourselves.
        if s.sch == sch && !s.channel.is_empty() && s.channel_id == cid {
            let now = Instant::now();
            for (id, _) in &channel {
                if !s.channel.iter().any(|c| c.0 == *id) {
                    s.joined.insert(*id, now);
                }
            }
        }
        s.joined.retain(|id, _| channel.iter().any(|c| c.0 == *id));
        s.channel_id = cid;
        if s.host != host {
            if let Some(h) = &host {
                log(&format!("вкладка {current}: сервер {h}"));
            }
            s.host = host;
        }
        (s.sch, s.self_id, s.channel) = (sch, me, channel);
    }
    overlay::refresh();
}

// ---- Required plugin exports ----

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_name() -> *const c_char {
    c"TS Talk Overlay".as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_version() -> *const c_char {
    c"0.3.0".as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_apiVersion() -> c_int {
    26
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_author() -> *const c_char {
    c"TakeChance".as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_description() -> *const c_char {
    c"Overlays: who is talking (whisper / channel) and who is in the channel".as_ptr()
}

/// TS passes `struct TS3Functions` by value; on x64 that arrives as a pointer to a temporary
/// copy, so the pointers we need are copied out here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ts3plugin_setFunctionPointers(funcs: *const *const c_void) {
    unsafe {
        let _ = TS3.set(Ts3Fns {
            free_memory: std::mem::transmute(*funcs.add(5)),
            set_client_volume_modifier: std::mem::transmute(*funcs.add(39)),
            get_client_id: std::mem::transmute(*funcs.add(73)),
            get_client_variable_as_int: std::mem::transmute(*funcs.add(79)),
            get_client_variable_as_string: std::mem::transmute(*funcs.add(81)),
            get_channel_of_client: std::mem::transmute(*funcs.add(83)),
            get_channel_client_list: std::mem::transmute(*funcs.add(94)),
            get_current_server_connection_handler_id: std::mem::transmute(*funcs.add(183)),
            get_server_connect_info: std::mem::transmute(*funcs.add(189)),
        });
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_init() -> c_int {
    overlay::start();
    refresh_channel();
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_shutdown() {
    overlay::stop();
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_offersConfigure() -> c_int {
    1 // PLUGIN_OFFERS_CONFIGURE_NEW_THREAD
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_configure(_handle: *mut c_void, _q_parent_widget: *mut c_void) {
    settings::show();
}

// ---- Events ----

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_currentServerConnectionChanged(_sch: u64) {
    refresh_channel();
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onConnectStatusChangeEvent(_sch: u64, new_status: c_int, _error: c_uint) {
    if new_status == STATUS_CONNECTION_ESTABLISHED || new_status == STATUS_DISCONNECTED {
        refresh_channel();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onTalkStatusChangeEvent(sch: u64, status: c_int, is_received_whisper: c_int, client_id: u16) {
    let talking = status == STATUS_TALKING;
    let (name, vol) = if talking { (nickname(sch, client_id), volume(sch, client_id)) } else { (String::new(), 0) };
    {
        let mut s = lock(&STATE);
        if s.sch != sch {
            return;
        }
        if talking {
            let speaker = Speaker { name, talking, whisper: is_received_whisper != 0, since: Instant::now(), volume: vol };
            s.speakers.insert(client_id, speaker);
        } else if let Some(sp) = s.speakers.get_mut(&client_id) {
            sp.talking = false;
            sp.since = Instant::now(); // starts the linger countdown
        }
    }
    overlay::refresh();
}

fn on_move(sch: u64, client_id: u16, visibility: c_int) {
    {
        let mut s = lock(&STATE);
        if s.sch != sch {
            return;
        }
        if visibility == LEAVE_VISIBILITY {
            s.speakers.remove(&client_id); // clids get recycled
        }
    }
    refresh_channel();
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onClientMoveEvent(sch: u64, client_id: u16, _old: u64, _new: u64, visibility: c_int, _msg: *const c_char) {
    on_move(sch, client_id, visibility);
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onClientMoveTimeoutEvent(sch: u64, client_id: u16, _old: u64, _new: u64, visibility: c_int, _msg: *const c_char) {
    on_move(sch, client_id, visibility);
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onClientMoveMovedEvent(
    sch: u64, client_id: u16, _old: u64, _new: u64, visibility: c_int,
    _mover_id: u16, _mover_name: *const c_char, _mover_uid: *const c_char, _msg: *const c_char,
) {
    on_move(sch, client_id, visibility);
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onClientKickFromChannelEvent(
    sch: u64, client_id: u16, _old: u64, _new: u64, visibility: c_int,
    _kicker_id: u16, _kicker_name: *const c_char, _kicker_uid: *const c_char, _msg: *const c_char,
) {
    on_move(sch, client_id, visibility);
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onClientKickFromServerEvent(
    sch: u64, client_id: u16, _old: u64, _new: u64, visibility: c_int,
    _kicker_id: u16, _kicker_name: *const c_char, _kicker_uid: *const c_char, _msg: *const c_char,
) {
    on_move(sch, client_id, visibility);
}

#[unsafe(no_mangle)]
pub extern "C" fn ts3plugin_onClientDisplayNameChanged(sch: u64, client_id: u16, display_name: *const c_char, _uid: *const c_char) {
    if display_name.is_null() {
        return;
    }
    let name = unsafe { CStr::from_ptr(display_name) }.to_string_lossy().into_owned();
    {
        let mut s = lock(&STATE);
        if s.sch != sch {
            return;
        }
        if let Some(sp) = s.speakers.get_mut(&client_id) {
            sp.name = name.clone();
        }
        if let Some(c) = s.channel.iter_mut().find(|(id, _)| *id == client_id) {
            c.1 = name;
        }
    }
    overlay::refresh();
}
