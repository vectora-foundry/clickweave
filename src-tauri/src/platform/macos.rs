use core_foundation::base::TCFType;
use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, EventField, KeyCode,
};
use foreign_types::ForeignType;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use tokio::sync::mpsc;
use tracing::{error, info};

use super::{CaptureCommand, CaptureEvent, CaptureEventKind, MouseButton};

/// Handle for a running macOS event tap.
///
/// The tap runs on a dedicated std::thread with its own CFRunLoop.
/// Events are sent through a tokio mpsc channel to the async processing loop.
pub struct MacOSEventTap {
    thread: Option<std::thread::JoinHandle<()>>,
    paused: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    /// Raw CFRunLoopRef for the tap thread, used to wake/stop the run loop
    /// from the control thread. `CFRunLoopStop` is thread-safe.
    run_loop: Arc<AtomicPtr<std::ffi::c_void>>,
}

impl MacOSEventTap {
    /// Start a passive (listen-only) event tap on a background thread.
    ///
    /// Returns the tap handle and a receiver for captured events.
    /// The tap captures: mouse clicks, key down events, scroll wheel events.
    pub fn start() -> Result<(Self, mpsc::UnboundedReceiver<CaptureEvent>), String> {
        let (tx, rx) = mpsc::unbounded_channel();
        let paused = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));
        let run_loop = Arc::new(AtomicPtr::new(std::ptr::null_mut()));

        let paused_clone = paused.clone();
        let stopped_clone = stopped.clone();
        let run_loop_clone = run_loop.clone();

        // Oneshot for the tap thread to signal whether initialization succeeded.
        let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<(), String>>();

        let thread = std::thread::Builder::new()
            .name("walkthrough-event-tap".into())
            .spawn(move || {
                run_event_tap(tx, paused_clone, stopped_clone, run_loop_clone, init_tx);
            })
            .map_err(|e| format!("Failed to spawn event tap thread: {e}"))?;

        // Wait for the tap thread to report init success or failure.
        let init_result = init_rx
            .recv()
            .map_err(|_| "Event tap thread exited before reporting init status".to_string())?;
        init_result?;

        Ok((
            Self {
                thread: Some(thread),
                paused,
                stopped,
                run_loop,
            },
            rx,
        ))
    }

    pub fn send_command(&self, cmd: CaptureCommand) {
        match cmd {
            CaptureCommand::Pause => self.paused.store(true, Ordering::SeqCst),
            CaptureCommand::Resume => self.paused.store(false, Ordering::SeqCst),
            CaptureCommand::Stop => {
                self.stopped.store(true, Ordering::SeqCst);
                self.wake_run_loop();
            }
        }
    }

    /// Stop the tap thread's CFRunLoop from any thread.
    /// `CFRunLoopStop` is thread-safe.
    fn wake_run_loop(&self) {
        let rl = self.run_loop.load(Ordering::SeqCst);
        if !rl.is_null() {
            unsafe {
                CFRunLoopStop(rl as *const _);
            }
        }
    }
}

impl Drop for MacOSEventTap {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.wake_run_loop();
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

fn run_event_tap(
    tx: mpsc::UnboundedSender<CaptureEvent>,
    paused: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    run_loop_out: Arc<AtomicPtr<std::ffi::c_void>>,
    init_tx: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let events_of_interest = vec![
        CGEventType::LeftMouseDown,
        CGEventType::RightMouseDown,
        CGEventType::KeyDown,
        CGEventType::ScrollWheel,
    ];

    let stopped_for_check = stopped.clone();
    let tap_result = CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::ListenOnly,
        events_of_interest,
        move |_proxy, event_type, event| {
            if stopped.load(Ordering::SeqCst) {
                CFRunLoop::get_current().stop();
                return None;
            }

            if paused.load(Ordering::SeqCst) {
                return None;
            }

            if let Some(kind) = translate_event(event_type, event) {
                let target_pid =
                    event.get_integer_value_field(EventField::EVENT_TARGET_UNIX_PROCESS_ID) as i32;
                let timestamp = clickweave_core::storage::now_millis();
                let capture_event = CaptureEvent {
                    kind,
                    target_pid,
                    timestamp,
                };
                if tx.send(capture_event).is_err() {
                    // Receiver dropped — stop the run loop.
                    CFRunLoop::get_current().stop();
                }
            }

            None // listen-only, don't modify events
        },
    );

    let tap = match tap_result {
        Ok(tap) => tap,
        Err(()) => {
            let msg = "Failed to create CGEvent tap. \
                 Ensure Accessibility permissions are granted in \
                 System Settings > Privacy & Security > Accessibility.";
            error!("{msg}");
            let _ = init_tx.send(Err(msg.to_string()));
            return;
        }
    };

    let loop_source = match tap.mach_port.create_runloop_source(0) {
        Ok(source) => source,
        Err(()) => {
            let msg = "Failed to create CFRunLoop source for event tap";
            error!("{msg}");
            let _ = init_tx.send(Err(msg.to_string()));
            return;
        }
    };

    let current_loop = CFRunLoop::get_current();
    unsafe {
        current_loop.add_source(&loop_source, kCFRunLoopCommonModes);
    }

    // Publish the run loop ref so the control thread can call CFRunLoopStop.
    let raw_rl = current_loop.as_concrete_TypeRef() as *mut std::ffi::c_void;
    run_loop_out.store(raw_rl, Ordering::SeqCst);

    // Signal successful initialization before entering the run loop.
    let _ = init_tx.send(Ok(()));

    // Check if stop was already requested before we got here.
    if stopped_for_check.load(Ordering::SeqCst) {
        info!("macOS event tap: stop requested before run loop started");
        return;
    }

    tap.enable();

    info!("macOS event tap started");
    CFRunLoop::run_current();
    info!("macOS event tap stopped");
}

fn translate_event(event_type: CGEventType, event: &CGEvent) -> Option<CaptureEventKind> {
    match event_type {
        CGEventType::LeftMouseDown | CGEventType::RightMouseDown => {
            let location = event.location();
            let click_count =
                event.get_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE) as u32;
            let button = match event_type {
                CGEventType::LeftMouseDown => MouseButton::Left,
                CGEventType::RightMouseDown => MouseButton::Right,
                _ => MouseButton::Center,
            };
            let modifiers = flags_to_modifiers(event.get_flags());

            Some(CaptureEventKind::MouseClick {
                x: location.x,
                y: location.y,
                button,
                click_count,
                modifiers,
            })
        }

        CGEventType::KeyDown => {
            // Skip auto-repeat events.
            let is_repeat =
                event.get_integer_value_field(EventField::KEYBOARD_EVENT_AUTOREPEAT) != 0;
            if is_repeat {
                return None;
            }

            let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
            let modifiers = flags_to_modifiers(event.get_flags());
            let key_name = keycode_to_name(keycode);
            let characters = get_unicode_string(event);

            Some(CaptureEventKind::KeyDown {
                key_name,
                characters,
                modifiers,
            })
        }

        CGEventType::ScrollWheel => {
            let delta_y = event
                .get_double_value_field(EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_1);
            let location = event.location();

            // Ignore tiny accidental scrolls.
            if delta_y.abs() < 0.5 {
                return None;
            }

            Some(CaptureEventKind::ScrollWheel {
                delta_y,
                x: location.x,
                y: location.y,
            })
        }

        _ => None,
    }
}

fn flags_to_modifiers(flags: CGEventFlags) -> Vec<String> {
    let mut mods = Vec::new();
    if flags.contains(CGEventFlags::CGEventFlagCommand) {
        mods.push("command".to_string());
    }
    if flags.contains(CGEventFlags::CGEventFlagShift) {
        mods.push("shift".to_string());
    }
    if flags.contains(CGEventFlags::CGEventFlagAlternate) {
        mods.push("option".to_string());
    }
    if flags.contains(CGEventFlags::CGEventFlagControl) {
        mods.push("control".to_string());
    }
    mods
}

/// Map a macOS virtual keycode to the key name accepted by the MCP `press_key` tool.
///
/// Names match the `key_name_to_code()` mapping in native-devtools-mcp
/// (`src/macos/input.rs`).
fn keycode_to_name(keycode: u16) -> String {
    match keycode {
        // Special keys.
        KeyCode::RETURN => "return".to_string(),
        KeyCode::TAB => "tab".to_string(),
        KeyCode::SPACE => "space".to_string(),
        KeyCode::DELETE => "delete".to_string(),
        KeyCode::ESCAPE => "escape".to_string(),
        KeyCode::LEFT_ARROW => "left".to_string(),
        KeyCode::RIGHT_ARROW => "right".to_string(),
        KeyCode::DOWN_ARROW => "down".to_string(),
        KeyCode::UP_ARROW => "up".to_string(),
        KeyCode::HOME => "home".to_string(),
        KeyCode::END => "end".to_string(),
        KeyCode::PAGE_UP => "pageup".to_string(),
        KeyCode::PAGE_DOWN => "pagedown".to_string(),
        KeyCode::FORWARD_DELETE => "forwarddelete".to_string(),
        // Function keys.
        KeyCode::F1 => "f1".to_string(),
        KeyCode::F2 => "f2".to_string(),
        KeyCode::F3 => "f3".to_string(),
        KeyCode::F4 => "f4".to_string(),
        KeyCode::F5 => "f5".to_string(),
        KeyCode::F6 => "f6".to_string(),
        KeyCode::F7 => "f7".to_string(),
        KeyCode::F8 => "f8".to_string(),
        KeyCode::F9 => "f9".to_string(),
        KeyCode::F10 => "f10".to_string(),
        KeyCode::F11 => "f11".to_string(),
        KeyCode::F12 => "f12".to_string(),
        // ANSI letter keys (US keyboard layout virtual keycodes).
        0x00 => "a".to_string(),
        0x01 => "s".to_string(),
        0x02 => "d".to_string(),
        0x03 => "f".to_string(),
        0x04 => "h".to_string(),
        0x05 => "g".to_string(),
        0x06 => "z".to_string(),
        0x07 => "x".to_string(),
        0x08 => "c".to_string(),
        0x09 => "v".to_string(),
        0x0B => "b".to_string(),
        0x0C => "q".to_string(),
        0x0D => "w".to_string(),
        0x0E => "e".to_string(),
        0x0F => "r".to_string(),
        0x10 => "y".to_string(),
        0x11 => "t".to_string(),
        0x12 => "1".to_string(),
        0x13 => "2".to_string(),
        0x14 => "3".to_string(),
        0x15 => "4".to_string(),
        0x16 => "6".to_string(),
        0x17 => "5".to_string(),
        0x18 => "=".to_string(),
        0x19 => "9".to_string(),
        0x1A => "7".to_string(),
        0x1B => "-".to_string(),
        0x1C => "8".to_string(),
        0x1D => "0".to_string(),
        0x1E => "]".to_string(),
        0x1F => "o".to_string(),
        0x20 => "u".to_string(),
        0x21 => "[".to_string(),
        0x22 => "i".to_string(),
        0x23 => "p".to_string(),
        0x25 => "l".to_string(),
        0x26 => "j".to_string(),
        0x27 => "'".to_string(),
        0x28 => "k".to_string(),
        0x29 => ";".to_string(),
        0x2A => "\\".to_string(),
        0x2B => ",".to_string(),
        0x2C => "/".to_string(),
        0x2D => "n".to_string(),
        0x2E => "m".to_string(),
        0x2F => ".".to_string(),
        0x32 => "`".to_string(),
        // Numpad keys.
        0x41 => "NumpadDecimal".to_string(),
        0x43 => "NumpadMultiply".to_string(),
        0x45 => "NumpadPlus".to_string(),
        0x47 => "NumpadClear".to_string(),
        0x4B => "NumpadDivide".to_string(),
        0x4C => "NumpadEnter".to_string(),
        0x4E => "NumpadMinus".to_string(),
        0x51 => "NumpadEquals".to_string(),
        0x52 => "Numpad0".to_string(),
        0x53 => "Numpad1".to_string(),
        0x54 => "Numpad2".to_string(),
        0x55 => "Numpad3".to_string(),
        0x56 => "Numpad4".to_string(),
        0x57 => "Numpad5".to_string(),
        0x58 => "Numpad6".to_string(),
        0x59 => "Numpad7".to_string(),
        0x5B => "Numpad8".to_string(),
        0x5C => "Numpad9".to_string(),
        _ => format!("0x{keycode:02X}"),
    }
}

/// Get the Unicode string associated with a key event.
///
/// Uses the raw `CGEventKeyboardGetUnicodeString` C API since the
/// `core-graphics` crate only exposes the setter, not the getter.
fn get_unicode_string(event: &CGEvent) -> Option<String> {
    let mut buf = [0u16; 8];
    let mut actual_len: u64 = 0;

    unsafe {
        CGEventKeyboardGetUnicodeString(
            event.as_ptr(),
            buf.len() as u64,
            &mut actual_len,
            buf.as_mut_ptr(),
        );
    }

    let len = actual_len as usize;
    if len == 0 {
        return None;
    }

    String::from_utf16(&buf[..len]).ok()
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventKeyboardGetUnicodeString(
        event: core_graphics::sys::CGEventRef,
        max_len: u64,
        actual_len: *mut u64,
        buf: *mut u16,
    );
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRunLoopStop(rl: *const std::ffi::c_void);
}

// ---------------------------------------------------------------------------
// Cursor region capture (bypasses MCP for low-overhead polling)
// ---------------------------------------------------------------------------

use core_graphics::display::{CGDisplay, CGPoint, CGRect, CGSize};
use core_graphics::window::{kCGNullWindowID, kCGWindowListOptionOnScreenOnly};

use super::{CursorRegionCapture, CURSOR_REGION_HALF_PT};

/// Get the current cursor position in screen coordinates (points).
pub fn get_cursor_position() -> (f64, f64) {
    unsafe {
        let event = CGEventCreate(std::ptr::null());
        if event.is_null() {
            return (0.0, 0.0);
        }
        let loc = CGEventGetLocation(event);
        CFRelease(event as *const _);
        (loc.x, loc.y)
    }
}

/// Capture a 64×64pt screen region centered on `(center_x, center_y)`.
///
/// Uses `CGDisplay::screenshot` with `kCGNullWindowID` to composite all
/// on-screen windows (cursor is NOT included). Returns `None` if the
/// capture fails.
pub fn capture_cursor_region(center_x: f64, center_y: f64) -> Option<CursorRegionCapture> {
    let rect = CGRect::new(
        &CGPoint::new(
            center_x - CURSOR_REGION_HALF_PT,
            center_y - CURSOR_REGION_HALF_PT,
        ),
        &CGSize::new(CURSOR_REGION_HALF_PT * 2.0, CURSOR_REGION_HALF_PT * 2.0),
    );

    let image = CGDisplay::screenshot(
        rect,
        kCGWindowListOptionOnScreenOnly,
        kCGNullWindowID,
        Default::default(),
    )?;

    let width = image.width();
    let height = image.height();
    if width == 0 || height == 0 {
        return None;
    }

    let rgba_bytes = cg_image_to_rgba(&image)?;

    Some(CursorRegionCapture {
        rgba_bytes,
        width: width as u32,
        height: height as u32,
    })
}

/// Convert a CGImage from BGRA to RGBA pixel bytes.
fn cg_image_to_rgba(cg_img: &core_graphics::image::CGImage) -> Option<Vec<u8>> {
    let width = cg_img.width() as u32;
    let height = cg_img.height() as u32;
    let bytes_per_row = cg_img.bytes_per_row();
    let data = cg_img.data();
    let raw = data.bytes();

    // CGImage uses BGRA; swap to RGBA.
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height as usize {
        let row_start = y * bytes_per_row;
        for x in 0..width as usize {
            let offset = row_start + x * 4;
            if offset + 3 < raw.len() {
                rgba.push(raw[offset + 2]); // R (from B)
                rgba.push(raw[offset + 1]); // G
                rgba.push(raw[offset]); // B (from R)
                rgba.push(raw[offset + 3]); // A
            }
        }
    }

    Some(rgba)
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreate(source: *const std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CGEventGetLocation(event: *mut std::ffi::c_void) -> CGPoint;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const std::ffi::c_void);
}
