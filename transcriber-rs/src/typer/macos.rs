//! macOS typing: paste through the general pasteboard with a synthetic Cmd+V,
//! then put back whatever the user had copied.
//!
//! macOS has no PRIMARY selection, so unlike the Wayland shift-insert path the
//! text has to go through the clipboard the user actually copies with. Every
//! item and type on the pasteboard is snapshotted first (images and file
//! references too, not just text) and restored after the paste settles — unless
//! something else wrote the pasteboard in the meantime, in which case the newer
//! contents win.
//!
//! Posting key events needs the Accessibility permission for the *responsible*
//! app (the terminal or launcher that started us). Without it CGEventPost drops
//! the event silently, so every paste checks first and fails loudly instead.

use std::ffi::c_void;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting};
use objc2_application_services::AXIsProcessTrusted;
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
};
use objc2_foundation::{NSArray, NSData, NSString};

use super::backends::{chunk_text, CHUNK_SIZE};

/// How long the focused app gets to read the pasteboard after Cmd+V before we
/// overwrite it (with the next chunk or the restored contents). Apps read it
/// while handling the key event; restoring too early pastes the old clipboard.
const PASTE_SETTLE: Duration = Duration::from_millis(200);

/// Whether this process may post synthetic key events.
pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Paste `text` (plus a trailing space) into the focused app via Cmd+V.
pub fn type_with_cmd_v(text: &str) -> Result<()> {
    if !accessibility_trusted() {
        anyhow::bail!(
            "Accessibility permission missing: Cmd+V would be dropped silently. Grant it to the \
             app that launched the transcriber (System Settings > Privacy & Security > \
             Accessibility)"
        );
    }

    let pasteboard = NSPasteboard::generalPasteboard();
    let saved = snapshot(&pasteboard);

    let text_with_space = format!("{text} ");
    let mut ours = pasteboard.changeCount();
    for chunk in chunk_text(&text_with_space, CHUNK_SIZE) {
        pasteboard.clearContents();
        let ok = unsafe {
            pasteboard.setString_forType(&NSString::from_str(chunk), NSPasteboardTypeString)
        };
        if !ok {
            anyhow::bail!("NSPasteboard rejected the transcript text");
        }
        ours = pasteboard.changeCount();
        post_cmd_v()?;
        std::thread::sleep(PASTE_SETTLE);
    }

    // Someone copied during the paste: their newer contents win over ours.
    if pasteboard.changeCount() == ours {
        restore(&pasteboard, &saved);
    } else {
        tracing::warn!("Clipboard changed during paste; not restoring the old contents");
    }
    Ok(())
}

/// One pasteboard item: every (type, data) pair it carried.
type SavedItem = Vec<(Retained<NSString>, Retained<NSData>)>;

fn snapshot(pasteboard: &NSPasteboard) -> Vec<SavedItem> {
    let Some(items) = pasteboard.pasteboardItems() else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            item.types()
                .iter()
                .filter_map(|ty| item.dataForType(&ty).map(|data| (ty, data)))
                .collect()
        })
        .collect()
}

fn restore(pasteboard: &NSPasteboard, saved: &[SavedItem]) {
    pasteboard.clearContents();
    if saved.is_empty() {
        return;
    }
    let items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = saved
        .iter()
        .map(|pairs| {
            let item = NSPasteboardItem::new();
            for (ty, data) in pairs {
                item.setData_forType(data, ty);
            }
            ProtocolObject::from_retained(item)
        })
        .collect();
    if !pasteboard.writeObjects(&NSArray::from_retained_slice(&items)) {
        tracing::warn!("Couldn't restore the previous clipboard contents after pasting");
    }
}

fn post_cmd_v() -> Result<()> {
    let keycode = keycode_for_char('v')?;
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    for key_down in [true, false] {
        let event = CGEvent::new_keyboard_event(source.as_deref(), keycode, key_down)
            .context("CGEventCreateKeyboardEvent returned null")?;
        CGEvent::set_flags(Some(&event), CGEventFlags::MaskCommand);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
    Ok(())
}

// Text Input Sources + Unicode key layout (Carbon/HIToolbox); no objc2 bindings exist.
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> *const c_void;
    fn TISGetInputSourceProperty(source: *const c_void, key: *const c_void) -> *const c_void;
    static kTISPropertyUnicodeKeyLayoutData: *const c_void;
    fn LMGetKbdType() -> u8;
    #[allow(clippy::too_many_arguments)]
    fn UCKeyTranslate(
        layout: *const c_void,
        virtual_key_code: u16,
        key_action: u16,
        modifier_key_state: u32,
        keyboard_type: u32,
        key_translate_options: u32,
        dead_key_state: *mut u32,
        max_string_length: usize,
        actual_string_length: *mut usize,
        unicode_string: *mut u16,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFRelease(cf: *const c_void);
}

const K_UC_KEY_ACTION_DISPLAY: u16 = 3;
const K_UC_KEY_TRANSLATE_NO_DEAD_KEYS: u32 = 1;

/// Physical keycode that types `ch` (unmodified) in the layout macOS uses for
/// Cmd shortcuts. Keycodes are positions, and Cmd+V means "the key that types
/// v": under plain Dvorak that's the QWERTY period key, not kVK_ANSI_V (which
/// would send Cmd+K). The ASCII-capable source is the one Cmd shortcuts use
/// when a non-Latin layout (e.g. Greek) is active. Looked up per paste so a
/// layout switch mid-session is honored.
fn keycode_for_char(ch: char) -> Result<u16> {
    unsafe {
        let source = TISCopyCurrentASCIICapableKeyboardLayoutInputSource();
        if source.is_null() {
            anyhow::bail!("no ASCII-capable keyboard layout is enabled");
        }
        let layout_data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
        let result = if layout_data.is_null() {
            Err(anyhow::anyhow!("current keyboard layout has no Unicode key layout data"))
        } else {
            let layout = CFDataGetBytePtr(layout_data) as *const c_void;
            let kbd_type = u32::from(LMGetKbdType());
            (0u16..128)
                .find(|&code| {
                    let mut dead_keys = 0u32;
                    let mut buf = [0u16; 4];
                    let mut len = 0usize;
                    let status = UCKeyTranslate(
                        layout,
                        code,
                        K_UC_KEY_ACTION_DISPLAY,
                        0,
                        kbd_type,
                        K_UC_KEY_TRANSLATE_NO_DEAD_KEYS,
                        &mut dead_keys,
                        buf.len(),
                        &mut len,
                        buf.as_mut_ptr(),
                    );
                    status == 0 && len == 1 && buf[0] == ch as u16
                })
                .with_context(|| format!("no key types {ch:?} in the current keyboard layout"))
        };
        CFRelease(source);
        result
    }
}

/// Focused-app lookup via `lsappinfo` (bundle ID, e.g. `net.kovidgoyal.kitty`).
///
/// NSWorkspace's frontmostApplication would avoid the subprocesses, but it is
/// only refreshed by notifications on a running main run loop, which this
/// process doesn't have — it would keep reporting the app focused at startup.
pub struct WindowDetector;

impl WindowDetector {
    pub fn new() -> Self {
        Self
    }

    /// Lowercased bundle ID of the frontmost app. `Ok("")` when no app is
    /// frontmost (e.g. a bare desktop right after login).
    pub fn focused_window_class(&mut self) -> Result<String> {
        let front = lsappinfo(&["front"])?;
        let front = front.trim();
        if front.is_empty() || front == "[ NULL ]" {
            return Ok(String::new());
        }
        let info = lsappinfo(&["info", "-only", "bundleID", front])?;
        // Output: "CFBundleIdentifier"="net.kovidgoyal.kitty"
        let bundle_id = info
            .trim()
            .split_once('=')
            .map(|(_, v)| v.trim_matches('"'))
            .with_context(|| format!("unexpected lsappinfo output: {:?}", info.trim()))?;
        Ok(bundle_id.to_lowercase())
    }
}

fn lsappinfo(args: &[&str]) -> Result<String> {
    let out = Command::new("lsappinfo")
        .args(args)
        .output()
        .context("spawn lsappinfo")?;
    if !out.status.success() {
        anyhow::bail!(
            "lsappinfo {} exited with {}: {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs on a test worker thread, which also checks the TIS calls are fine
    /// off the main thread (the typer calls them from spawn_blocking).
    #[test]
    fn finds_a_keycode_for_v_in_the_current_layout() {
        let code = keycode_for_char('v').unwrap();
        assert!(code < 128);
        println!("v -> keycode {code}");
    }
}
