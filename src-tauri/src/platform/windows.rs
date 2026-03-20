use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use tracing::info;

use super::{CaptureCommand, CaptureEvent, CaptureEventKind, MouseButton};

#[cfg(target_os = "windows")]
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
#[cfg(target_os = "windows")]
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
#[cfg(target_os = "windows")]
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
#[cfg(target_os = "windows")]
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// Half-size of the cursor region capture in screen points.
/// 32pt → 64pt total region around the cursor.
#[allow(dead_code)]
pub const CURSOR_REGION_HALF_PT: f64 = 32.0;

/// A small screen region captured around the cursor position.
///
/// Stores raw RGBA pixels. The captured region IS the click crop template —
/// no secondary crop step is needed.
#[allow(dead_code)]
#[derive(Clone)]
pub struct CursorRegionCapture {
    /// Raw RGBA pixel data (4 bytes per pixel, row-major, top-down).
    pub rgba_bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

// ---------------------------------------------------------------------------
// Virtual key → name mapping
// ---------------------------------------------------------------------------

/// Map a Windows virtual key code to the key name accepted by the MCP
/// `press_key` tool.
///
/// Names mirror the macOS `keycode_to_name()` mapping so that recorded
/// walkthrough events are platform-agnostic at the consumer level.
#[allow(dead_code)]
pub fn vk_to_name(vk: u16) -> String {
    match vk {
        // Special keys.
        0x0D => "return".to_string(),   // VK_RETURN
        0x09 => "tab".to_string(),      // VK_TAB
        0x20 => "space".to_string(),    // VK_SPACE
        0x08 => "delete".to_string(),   // VK_BACK (backspace → "delete" matching macOS)
        0x1B => "escape".to_string(),   // VK_ESCAPE
        0x25 => "left".to_string(),     // VK_LEFT
        0x26 => "up".to_string(),       // VK_UP
        0x27 => "right".to_string(),    // VK_RIGHT
        0x28 => "down".to_string(),     // VK_DOWN
        0x24 => "home".to_string(),     // VK_HOME
        0x23 => "end".to_string(),      // VK_END
        0x21 => "pageup".to_string(),   // VK_PRIOR
        0x22 => "pagedown".to_string(), // VK_NEXT
        0x2E => "forwarddelete".to_string(), // VK_DELETE (forward delete)

        // Function keys.
        0x70 => "f1".to_string(),
        0x71 => "f2".to_string(),
        0x72 => "f3".to_string(),
        0x73 => "f4".to_string(),
        0x74 => "f5".to_string(),
        0x75 => "f6".to_string(),
        0x76 => "f7".to_string(),
        0x77 => "f8".to_string(),
        0x78 => "f9".to_string(),
        0x79 => "f10".to_string(),
        0x7A => "f11".to_string(),
        0x7B => "f12".to_string(),

        // Letter keys (VK_A–VK_Z are 0x41–0x5A).
        0x41..=0x5A => {
            let ch = (b'a' + (vk as u8 - 0x41)) as char;
            ch.to_string()
        }

        // Digit keys (VK_0–VK_9 are 0x30–0x39).
        0x30..=0x39 => {
            let ch = (b'0' + (vk as u8 - 0x30)) as char;
            ch.to_string()
        }

        // Numpad digit keys (VK_NUMPAD0–VK_NUMPAD9 are 0x60–0x69).
        0x60..=0x69 => {
            let digit = vk - 0x60;
            format!("Numpad{digit}")
        }
        0x6A => "NumpadMultiply".to_string(), // VK_MULTIPLY
        0x6B => "NumpadPlus".to_string(),     // VK_ADD
        0x6D => "NumpadMinus".to_string(),    // VK_SUBTRACT
        0x6E => "NumpadDecimal".to_string(),  // VK_DECIMAL
        0x6F => "NumpadDivide".to_string(),   // VK_DIVIDE

        // OEM keys (US layout).
        0xBA => ";".to_string(),  // VK_OEM_1
        0xBB => "=".to_string(),  // VK_OEM_PLUS
        0xBC => ",".to_string(),  // VK_OEM_COMMA
        0xBD => "-".to_string(),  // VK_OEM_MINUS
        0xBE => ".".to_string(),  // VK_OEM_PERIOD
        0xBF => "/".to_string(),  // VK_OEM_2
        0xC0 => "`".to_string(),  // VK_OEM_3
        0xDB => "[".to_string(),  // VK_OEM_4
        0xDC => "\\".to_string(), // VK_OEM_5
        0xDD => "]".to_string(),  // VK_OEM_6
        0xDE => "'".to_string(),  // VK_OEM_7

        // Unknown key: emit hex code so nothing is silently dropped.
        _ => format!("0x{vk:02X}"),
    }
}

// ---------------------------------------------------------------------------
// Multi-click tracker
// ---------------------------------------------------------------------------

/// Tracks consecutive clicks to compute click count (single, double, triple…).
///
/// Two clicks are considered consecutive when they target the same mouse button,
/// occur within 500 ms of each other, and land within 4 px of each other.
#[allow(dead_code)]
pub struct ClickTracker {
    last_button: u32,
    last_x: i32,
    last_y: i32,
    last_time: u64,
    count: u32,
}

impl ClickTracker {
    /// Create a new tracker with no previous click recorded.
    pub fn new() -> Self {
        Self {
            last_button: u32::MAX,
            last_x: 0,
            last_y: 0,
            last_time: 0,
            count: 0,
        }
    }

    /// Register a click and return the current consecutive click count.
    ///
    /// `timestamp_ms` is milliseconds since some monotonic or epoch origin;
    /// only differences between timestamps matter.
    pub fn register_click(&mut self, button: u32, x: i32, y: i32, timestamp_ms: u64) -> u32 {
        const MAX_INTERVAL_MS: u64 = 500;
        const MAX_DISTANCE_PX: i32 = 4;

        let same_button = button == self.last_button;
        let within_time = timestamp_ms.saturating_sub(self.last_time) <= MAX_INTERVAL_MS;
        let dx = (x - self.last_x).abs();
        let dy = (y - self.last_y).abs();
        let within_distance = dx <= MAX_DISTANCE_PX && dy <= MAX_DISTANCE_PX;

        if same_button && within_time && within_distance {
            self.count += 1;
        } else {
            self.count = 1;
        }

        self.last_button = button;
        self.last_x = x;
        self.last_y = y;
        self.last_time = timestamp_ms;

        self.count
    }
}

// ---------------------------------------------------------------------------
// Pixel format conversion
// ---------------------------------------------------------------------------

/// Convert BGRA bottom-up pixels (GDI `GetDIBits` format) to RGBA top-down
/// pixels suitable for use with image encoders and the MCP screenshot path.
///
/// `bgra` must contain exactly `width * height * 4` bytes.
#[allow(dead_code)]
pub fn bgra_bottom_up_to_rgba(bgra: &[u8], width: u32, height: u32) -> Vec<u8> {
    let row_bytes = (width * 4) as usize;
    let mut rgba = Vec::with_capacity(bgra.len());

    // GDI bottom-up: row 0 in the buffer is the bottom row of the image.
    // Iterate rows in reverse to flip to top-down.
    for row in (0..height as usize).rev() {
        let start = row * row_bytes;
        let end = start + row_bytes;
        let row_slice = &bgra[start..end];
        for pixel in row_slice.chunks_exact(4) {
            rgba.push(pixel[2]); // R (from B at index 2)
            rgba.push(pixel[1]); // G
            rgba.push(pixel[0]); // B (from R at index 0)
            rgba.push(pixel[3]); // A
        }
    }

    rgba
}

// ---------------------------------------------------------------------------
// Windows event hook — low-level mouse and keyboard hooks
// ---------------------------------------------------------------------------

/// Per-thread state shared between the hook thread functions and the thread
/// lifecycle management code. Stored in a thread-local to avoid needing a
/// global pointer while remaining accessible from `extern "system"` callbacks
/// (which cannot capture closures).
#[cfg(target_os = "windows")]
struct HookThreadState {
    tx: mpsc::UnboundedSender<CaptureEvent>,
    paused: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    mouse_hook: HHOOK,
    keyboard_hook: HHOOK,
    click_tracker: ClickTracker,
    held_keys: HashSet<u16>,
}

#[cfg(target_os = "windows")]
thread_local! {
    static HOOK_STATE: RefCell<Option<HookThreadState>> = const { RefCell::new(None) };
}

/// Handle for a running Windows low-level input hook pair (mouse + keyboard).
///
/// Hooks run on a dedicated `std::thread` with its own Win32 message pump.
/// Events are sent through a tokio mpsc channel to the async processing loop.
#[allow(dead_code)]
#[cfg(target_os = "windows")]
pub struct WindowsEventHook {
    thread: Option<std::thread::JoinHandle<()>>,
    thread_id: u32,
    paused: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
}

#[cfg(target_os = "windows")]
impl WindowsEventHook {
    /// Start mouse and keyboard hooks on a background thread.
    ///
    /// Returns the hook handle and a receiver for captured events.
    pub fn start() -> Result<(Self, mpsc::UnboundedReceiver<CaptureEvent>), String> {
        let (tx, rx) = mpsc::unbounded_channel();
        let paused = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));

        let paused_clone = paused.clone();
        let stopped_clone = stopped.clone();

        // One-shot channel: hook thread reports its thread ID or an error.
        let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<u32, String>>();

        let thread = std::thread::Builder::new()
            .name("walkthrough-event-hook".into())
            .spawn(move || {
                run_event_hooks(tx, paused_clone, stopped_clone, init_tx);
            })
            .map_err(|e| format!("Failed to spawn event hook thread: {e}"))?;

        let thread_id = init_rx
            .recv()
            .map_err(|_| "Event hook thread exited before reporting init status".to_string())??;

        Ok((
            Self {
                thread: Some(thread),
                thread_id,
                paused,
                stopped,
            },
            rx,
        ))
    }

    /// Send a control command to the hook thread.
    pub fn send_command(&self, cmd: CaptureCommand) {
        match cmd {
            CaptureCommand::Pause => self.paused.store(true, Ordering::SeqCst),
            CaptureCommand::Resume => self.paused.store(false, Ordering::SeqCst),
            CaptureCommand::Stop => {
                self.stopped.store(true, Ordering::SeqCst);
                // Wake the message pump so it can exit.
                unsafe {
                    PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
                }
            }
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsEventHook {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        unsafe {
            PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

/// Thread function: installs hooks, runs message pump, cleans up on exit.
#[cfg(target_os = "windows")]
fn run_event_hooks(
    tx: mpsc::UnboundedSender<CaptureEvent>,
    paused: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    init_tx: std::sync::mpsc::Sender<Result<u32, String>>,
) {
    let thread_id = unsafe { GetCurrentThreadId() };

    // Install low-level mouse hook.
    let mouse_hook = unsafe {
        SetWindowsHookExW(
            WH_MOUSE_LL,
            Some(mouse_hook_proc),
            std::ptr::null_mut(),
            0,
        )
    };
    if mouse_hook.is_null() {
        let _ = init_tx.send(Err("Failed to install WH_MOUSE_LL hook".to_string()));
        return;
    }

    // Install low-level keyboard hook.
    let keyboard_hook = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(keyboard_hook_proc),
            std::ptr::null_mut(),
            0,
        )
    };
    if keyboard_hook.is_null() {
        unsafe { UnhookWindowsHookEx(mouse_hook) };
        let _ = init_tx.send(Err("Failed to install WH_KEYBOARD_LL hook".to_string()));
        return;
    }

    // Store per-thread state so hook callbacks can access it.
    HOOK_STATE.with(|cell| {
        *cell.borrow_mut() = Some(HookThreadState {
            tx,
            paused,
            stopped,
            mouse_hook,
            keyboard_hook,
            click_tracker: ClickTracker::new(),
            held_keys: HashSet::new(),
        });
    });

    // Signal successful initialization with our thread ID.
    let _ = init_tx.send(Ok(thread_id));

    info!("Windows event hooks installed (thread {})", thread_id);

    // Message pump — GetMessageW returns 0 when WM_QUIT is received.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let ret = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if ret == 0 || ret == -1 {
            break;
        }
    }

    // Cleanup: remove hooks and clear thread-local state.
    unsafe {
        UnhookWindowsHookEx(mouse_hook);
        UnhookWindowsHookEx(keyboard_hook);
    }
    HOOK_STATE.with(|cell| {
        *cell.borrow_mut() = None;
    });

    info!("Windows event hooks removed (thread {})", thread_id);
}

/// Low-level mouse hook callback.
#[cfg(target_os = "windows")]
unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: lparam points to a MSLLHOOKSTRUCT when code >= 0 for low-level mouse hooks.
        let hook_struct = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };

        let should_process = HOOK_STATE.with(|cell| {
            let state = cell.borrow();
            if let Some(s) = state.as_ref() {
                !s.paused.load(Ordering::SeqCst) && !s.stopped.load(Ordering::SeqCst)
            } else {
                false
            }
        });

        if should_process {
            let msg = wparam as u32;
            match msg {
                WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                    let button = match msg {
                        WM_LBUTTONDOWN => MouseButton::Left,
                        WM_RBUTTONDOWN => MouseButton::Right,
                        _ => MouseButton::Center,
                    };
                    let x = hook_struct.pt.x as f64;
                    let y = hook_struct.pt.y as f64;
                    let timestamp = clickweave_core::storage::now_millis();

                    HOOK_STATE.with(|cell| {
                        let mut state = cell.borrow_mut();
                        if let Some(s) = state.as_mut() {
                            let click_count = s.click_tracker.register_click(
                                msg,
                                hook_struct.pt.x,
                                hook_struct.pt.y,
                                hook_struct.time as u64,
                            );
                            let modifiers = get_modifiers();
                            let target_pid = get_foreground_pid();
                            let event = CaptureEvent {
                                kind: CaptureEventKind::MouseClick {
                                    x,
                                    y,
                                    button,
                                    click_count,
                                    modifiers,
                                },
                                target_pid,
                                timestamp,
                            };
                            let _ = s.tx.send(event);
                        }
                    });
                }
                WM_MOUSEWHEEL => {
                    // High word of mouseData is the signed wheel delta.
                    let raw_delta = (hook_struct.mouseData >> 16) as i16;
                    let delta_y = -(raw_delta as f64) / WHEEL_DELTA as f64;
                    if delta_y.abs() >= 0.5 {
                        let x = hook_struct.pt.x as f64;
                        let y = hook_struct.pt.y as f64;
                        let timestamp = clickweave_core::storage::now_millis();
                        let target_pid = get_foreground_pid();
                        HOOK_STATE.with(|cell| {
                            let state = cell.borrow();
                            if let Some(s) = state.as_ref() {
                                let event = CaptureEvent {
                                    kind: CaptureEventKind::ScrollWheel { delta_y, x, y },
                                    target_pid,
                                    timestamp,
                                };
                                let _ = s.tx.send(event);
                            }
                        });
                    }
                }
                // WM_XBUTTONDOWN and WM_MOUSEHWHEEL are intentionally ignored.
                _ => {}
            }
        }
    }

    // SAFETY: standard hook chain forwarding.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Low-level keyboard hook callback.
#[cfg(target_os = "windows")]
unsafe extern "system" fn keyboard_hook_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code >= 0 {
        // SAFETY: lparam points to a KBDLLHOOKSTRUCT when code >= 0 for low-level keyboard hooks.
        let hook_struct = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let vk = hook_struct.vkCode as u16;
        let msg = wparam as u32;

        // Maintain held_keys for auto-repeat detection.
        if msg == WM_KEYUP || msg == WM_SYSKEYUP {
            HOOK_STATE.with(|cell| {
                let mut state = cell.borrow_mut();
                if let Some(s) = state.as_mut() {
                    s.held_keys.remove(&vk);
                }
            });
        } else if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
            // Skip injected events.
            if hook_struct.flags & LLKHF_INJECTED != 0 {
                // SAFETY: standard hook chain forwarding.
                return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
            }

            let paused = HOOK_STATE.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .map_or(true, |s| s.paused.load(Ordering::SeqCst))
            });
            if paused {
                // SAFETY: standard hook chain forwarding.
                return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
            }

            // Auto-repeat filter: skip if key is already held.
            let already_held = HOOK_STATE.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .map_or(false, |s| s.held_keys.contains(&vk))
            });
            if already_held {
                // SAFETY: standard hook chain forwarding.
                return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
            }

            // Mark key as held.
            HOOK_STATE.with(|cell| {
                let mut state = cell.borrow_mut();
                if let Some(s) = state.as_mut() {
                    s.held_keys.insert(vk);
                }
            });

            let key_name = vk_to_name(vk);
            let characters = get_unicode_char(vk as u32, hook_struct.scanCode);
            let modifiers = get_modifiers();
            let target_pid = get_foreground_pid();
            let timestamp = clickweave_core::storage::now_millis();

            HOOK_STATE.with(|cell| {
                let state = cell.borrow();
                if let Some(s) = state.as_ref() {
                    let event = CaptureEvent {
                        kind: CaptureEventKind::KeyDown {
                            key_name,
                            characters,
                            modifiers,
                        },
                        target_pid,
                        timestamp,
                    };
                    let _ = s.tx.send(event);
                }
            });
        }
    }

    // SAFETY: standard hook chain forwarding.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Returns the list of currently held modifier key names.
#[cfg(target_os = "windows")]
fn get_modifiers() -> Vec<String> {
    let mut mods = Vec::new();
    unsafe {
        if GetAsyncKeyState(VK_SHIFT as i32) as u16 & 0x8000 != 0 {
            mods.push("shift".to_string());
        }
        if GetAsyncKeyState(VK_CONTROL as i32) as u16 & 0x8000 != 0 {
            mods.push("control".to_string());
        }
        if GetAsyncKeyState(VK_MENU as i32) as u16 & 0x8000 != 0 {
            mods.push("alt".to_string());
        }
        if GetAsyncKeyState(VK_LWIN as i32) as u16 & 0x8000 != 0
            || GetAsyncKeyState(VK_RWIN as i32) as u16 & 0x8000 != 0
        {
            mods.push("command".to_string());
        }
    }
    mods
}

/// Returns the PID of the process owning the current foreground window.
#[cfg(target_os = "windows")]
fn get_foreground_pid() -> i32 {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return 0;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        pid as i32
    }
}

/// Returns the Unicode character(s) produced by the given virtual key and
/// scan code, using the current keyboard state.
#[cfg(target_os = "windows")]
fn get_unicode_char(vk: u32, scan_code: u32) -> Option<String> {
    let mut keyboard_state = [0u8; 256];
    let mut buf = [0u16; 8];

    let result = unsafe {
        GetKeyboardState(keyboard_state.as_mut_ptr());
        ToUnicode(
            vk,
            scan_code,
            keyboard_state.as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as i32,
            0,
        )
    };

    if result <= 0 {
        return None;
    }

    String::from_utf16(&buf[..result as usize]).ok().and_then(|s| {
        // Filter out control characters (e.g., from arrow keys, function keys).
        if s.chars().all(|c| c.is_control()) {
            None
        } else {
            Some(s)
        }
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- vk_to_name ----------------------------------------------------------

    #[test]
    fn vk_to_name_return_key() {
        assert_eq!(vk_to_name(0x0D), "return");
    }

    #[test]
    fn vk_to_name_letter_keys() {
        assert_eq!(vk_to_name(0x41), "a");
        assert_eq!(vk_to_name(0x5A), "z");
        assert_eq!(vk_to_name(0x4D), "m");
    }

    #[test]
    fn vk_to_name_number_keys() {
        assert_eq!(vk_to_name(0x30), "0");
        assert_eq!(vk_to_name(0x39), "9");
        assert_eq!(vk_to_name(0x35), "5");
    }

    #[test]
    fn vk_to_name_arrow_keys() {
        assert_eq!(vk_to_name(0x25), "left");
        assert_eq!(vk_to_name(0x26), "up");
        assert_eq!(vk_to_name(0x27), "right");
        assert_eq!(vk_to_name(0x28), "down");
    }

    #[test]
    fn vk_to_name_function_keys() {
        assert_eq!(vk_to_name(0x70), "f1");
        assert_eq!(vk_to_name(0x7B), "f12");
        assert_eq!(vk_to_name(0x75), "f6");
    }

    #[test]
    fn vk_to_name_special_keys() {
        assert_eq!(vk_to_name(0x09), "tab");
        assert_eq!(vk_to_name(0x20), "space");
        assert_eq!(vk_to_name(0x08), "delete");
        assert_eq!(vk_to_name(0x1B), "escape");
        assert_eq!(vk_to_name(0x24), "home");
        assert_eq!(vk_to_name(0x23), "end");
        assert_eq!(vk_to_name(0x21), "pageup");
        assert_eq!(vk_to_name(0x22), "pagedown");
        assert_eq!(vk_to_name(0x2E), "forwarddelete");
    }

    #[test]
    fn vk_to_name_unknown_emits_hex() {
        assert_eq!(vk_to_name(0xFF), "0xFF");
        assert_eq!(vk_to_name(0x01), "0x01");
    }

    // --- ClickTracker --------------------------------------------------------

    #[test]
    fn click_tracker_first_click_returns_one() {
        let mut tracker = ClickTracker::new();
        let count = tracker.register_click(0, 100, 200, 1000);
        assert_eq!(count, 1);
    }

    #[test]
    fn click_tracker_double_click_same_position() {
        let mut tracker = ClickTracker::new();
        tracker.register_click(0, 100, 200, 1000);
        let count = tracker.register_click(0, 100, 200, 1200);
        assert_eq!(count, 2);
    }

    #[test]
    fn click_tracker_triple_click_same_position() {
        let mut tracker = ClickTracker::new();
        tracker.register_click(0, 100, 200, 1000);
        tracker.register_click(0, 100, 200, 1200);
        let count = tracker.register_click(0, 100, 200, 1400);
        assert_eq!(count, 3);
    }

    #[test]
    fn click_tracker_resets_after_timeout() {
        let mut tracker = ClickTracker::new();
        tracker.register_click(0, 100, 200, 1000);
        // 501 ms later — exceeds the 500 ms threshold.
        let count = tracker.register_click(0, 100, 200, 1501);
        assert_eq!(count, 1);
    }

    #[test]
    fn click_tracker_resets_after_large_move() {
        let mut tracker = ClickTracker::new();
        tracker.register_click(0, 100, 200, 1000);
        // Moved more than 4 px.
        let count = tracker.register_click(0, 110, 200, 1200);
        assert_eq!(count, 1);
    }

    #[test]
    fn click_tracker_resets_on_different_button() {
        let mut tracker = ClickTracker::new();
        tracker.register_click(0, 100, 200, 1000);
        // Different button (right click).
        let count = tracker.register_click(1, 100, 200, 1200);
        assert_eq!(count, 1);
    }

    #[test]
    fn click_tracker_allows_small_move_within_threshold() {
        let mut tracker = ClickTracker::new();
        tracker.register_click(0, 100, 200, 1000);
        // Moved exactly 4 px — still within threshold.
        let count = tracker.register_click(0, 104, 204, 1200);
        assert_eq!(count, 2);
    }

    // --- bgra_bottom_up_to_rgba ----------------------------------------------

    #[test]
    fn bgra_bottom_up_to_rgba_2x2_image() {
        // 2×2 image, bottom-up BGRA:
        //   Row 0 (bottom of image): pixel (0,1) = BGRA(10,20,30,255), pixel (1,1) = BGRA(40,50,60,255)
        //   Row 1 (top of image):    pixel (0,0) = BGRA(70,80,90,255), pixel (1,0) = BGRA(100,110,120,255)
        #[rustfmt::skip]
        let bgra: Vec<u8> = vec![
            // Row 0 in buffer = bottom row of image
            10, 20, 30, 255,   // pixel (col=0, imageRow=1): B=10,G=20,R=30,A=255
            40, 50, 60, 255,   // pixel (col=1, imageRow=1): B=40,G=50,R=60,A=255
            // Row 1 in buffer = top row of image
            70, 80, 90, 255,   // pixel (col=0, imageRow=0): B=70,G=80,R=90,A=255
            100, 110, 120, 255,// pixel (col=1, imageRow=0): B=100,G=110,R=120,A=255
        ];

        let rgba = bgra_bottom_up_to_rgba(&bgra, 2, 2);

        // Expected output is top-down RGBA:
        // Top row (from buffer row 1): R=90,G=80,B=70,A=255  then R=120,G=110,B=100,A=255
        // Bottom row (from buffer row 0): R=30,G=20,B=10,A=255  then R=60,G=50,B=40,A=255
        #[rustfmt::skip]
        let expected: Vec<u8> = vec![
            90, 80, 70, 255,    // top-left
            120, 110, 100, 255, // top-right
            30, 20, 10, 255,    // bottom-left
            60, 50, 40, 255,    // bottom-right
        ];

        assert_eq!(rgba, expected);
    }

    #[test]
    fn bgra_bottom_up_to_rgba_preserves_length() {
        let bgra = vec![0u8; 4 * 3 * 5]; // 3×5 image
        let rgba = bgra_bottom_up_to_rgba(&bgra, 3, 5);
        assert_eq!(rgba.len(), bgra.len());
    }
}
