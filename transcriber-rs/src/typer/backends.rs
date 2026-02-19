use std::process::Command;

use anyhow::Result;

/// Chunk size: 801 chars shows actual text in Claude Code,
/// 802+ triggers "[Pasted text]" display.
const CHUNK_SIZE: usize = 801;

/// Characters that are unsafe at keycode position 22 in wtype.
const UNSAFE_AT_22: &str = " !\"#$'()*+,-./:;=>?@[\\]^_";

/// Dispatch to the appropriate typing backend.
pub fn type_with_adaptive(text: &str, method: &str) -> Result<()> {
    match method {
        "shift-insert" => type_with_shift_insert(text),
        "wtype" => type_with_wtype(text),
        "middle-click" => type_with_middle_click(text),
        "ydotool" => type_with_ydotool(text),
        _ => type_with_wtype(text),
    }
}

fn type_with_shift_insert(text: &str) -> Result<()> {
    let text_with_space = format!("{text} ");

    for chunk in chunk_text(&text_with_space, CHUNK_SIZE) {
        // Copy to PRIMARY selection
        let mut child = Command::new("wl-copy")
            .arg("--primary")
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        if let Some(stdin) = child.stdin.as_mut() {
            use std::io::Write;
            stdin.write_all(chunk.as_bytes())?;
        }
        child.wait()?;

        // Paste via Shift+Insert
        Command::new("wtype")
            .args(["-M", "shift", "-k", "Insert", "-m", "shift"])
            .output()?;
    }
    Ok(())
}

fn type_with_middle_click(text: &str) -> Result<()> {
    let text_with_space = format!("{text} ");

    for chunk in chunk_text(&text_with_space, CHUNK_SIZE) {
        // Copy to PRIMARY selection
        let mut child = Command::new("wl-copy")
            .args(["--primary", "--trim-newline"])
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        if let Some(stdin) = child.stdin.as_mut() {
            use std::io::Write;
            stdin.write_all(chunk.as_bytes())?;
        }
        child.wait()?;

        // Middle-click paste
        Command::new("wlrctl")
            .args(["pointer", "click", "middle"])
            .output()?;
    }
    Ok(())
}

fn type_with_ydotool(text: &str) -> Result<()> {
    let output = Command::new("ydotool")
        .args(["type", &format!("{text} ")])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.to_lowercase().contains("permission")
            || stderr.to_lowercase().contains("failed to connect")
        {
            eprintln!(
                "\n[WARNING] ydotool permission denied. Run: sudo chmod 666 /tmp/.ydotool_socket"
            );
        }
        anyhow::bail!("ydotool failed: {stderr}");
    }
    Ok(())
}

fn type_with_wtype(text: &str) -> Result<()> {
    let chunks = split_for_wtype_keycode22(&format!("{text} "));
    for chunk in &chunks {
        Command::new("wtype")
            .arg(chunk)
            .output()?;
    }
    Ok(())
}

/// Fix wtype keycode 22 bug where punctuation at position 14 triggers BackSpace.
///
/// wtype assigns keycodes starting at 9. If a punctuation char is the 14th
/// unique character (keycode 9+13=22), it's interpreted as BackSpace.
/// Split text so unsafe punct never lands at position 14.
fn split_for_wtype_keycode22(text: &str) -> Vec<String> {
    if text.is_empty() {
        return vec![];
    }

    let chars: Vec<char> = text.chars().collect();
    let mut chunks = Vec::new();
    let mut start = 0;

    while start < chars.len() {
        let mut seen = std::collections::HashSet::new();
        let mut pos14_index = None;
        let mut last_alnum_before_14 = None;

        for i in start..chars.len() {
            let ch = chars[i];
            if !seen.contains(&ch) {
                seen.insert(ch);
                if seen.len() == 14 {
                    pos14_index = Some(i);
                    break;
                }
            }
            if ch.is_alphanumeric() {
                last_alnum_before_14 = Some(i);
            }
        }

        if let Some(p14) = pos14_index {
            if UNSAFE_AT_22.contains(chars[p14]) {
                if let Some(split_at) = last_alnum_before_14 {
                    if split_at > start {
                        chunks.push(chars[start..split_at].iter().collect());
                        start = split_at;
                        continue;
                    }
                }
            }
        }

        chunks.push(chars[start..].iter().collect());
        break;
    }

    chunks
}

fn chunk_text(text: &str, chunk_size: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();

    while start < bytes.len() {
        let mut end = (start + chunk_size).min(bytes.len());
        // Don't split in the middle of a UTF-8 character
        while end < bytes.len() && !text.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(&text[start..end]);
        start = end;
    }

    chunks
}
