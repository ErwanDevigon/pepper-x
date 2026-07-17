use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode, SynchronizationCode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;
use xkbcommon::xkb;

const SOCKET_ENV: &str = "PEPPERX_UINPUT_HELPER_SOCKET";
const STARTUP_DELAY: Duration = Duration::from_millis(250);
const KEY_HOLD_DELAY: Duration = Duration::from_millis(2);
const INTER_KEY_DELAY: Duration = Duration::from_millis(1);
const DEAD_KEY_DELAY: Duration = Duration::from_millis(8);
const UNICODE_MODE_DELAY: Duration = Duration::from_millis(30);

/// Evdev keycodes start at 8 below XKB keycodes (XKB keycode = evdev keycode + 8).
const XKB_EVDEV_OFFSET: u32 = 8;

#[derive(Debug, Deserialize)]
struct UinputInsertRequest {
    text: String,
}

#[derive(Debug, Serialize)]
struct UinputInsertResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// One physical key press, optionally with Shift and/or AltGr (ISO Level3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeyChord {
    keycode: KeyCode,
    shift: bool,
    alt_gr: bool,
}

/// How to emit one character: layout chord sequence, or Unicode hex entry.
#[derive(Debug, Clone)]
enum CharStroke {
    /// One or more chords (single key, or dead-key + base).
    Chords(Vec<KeyChord>),
    /// Ctrl+Shift+U hex codepoint entry (GNOME/IBus/GTK).
    UnicodeHex(u32),
}

struct CharMapper {
    /// Preferred layout-based sequences (shortest wins at build time).
    map: HashMap<char, Vec<KeyChord>>,
    /// Digits/letters needed for Unicode hex entry.
    hex_digits: HashMap<char, KeyChord>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let socket_path = configured_socket_path()?;
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create helper socket directory: {error}"))?;
    }

    if socket_path.exists() {
        std::fs::remove_file(&socket_path)
            .map_err(|error| format!("failed to remove stale helper socket: {error}"))?;
    }

    let listener = UnixListener::bind(&socket_path).map_err(|error| {
        format!(
            "failed to bind helper socket {}: {error}",
            socket_path.display()
        )
    })?;

    let mapper = build_char_mapper_from_env()?;
    let mut device = create_virtual_keyboard(&mapper)?;

    loop {
        let (stream, _) = listener
            .accept()
            .map_err(|error| format!("failed to accept helper connection: {error}"))?;
        handle_connection(stream, &mut device, &mapper)?;
    }
}

fn configured_socket_path() -> Result<PathBuf, String> {
    if let Some(socket_path) = std::env::var_os(SOCKET_ENV) {
        return Ok(PathBuf::from(socket_path));
    }

    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| "PEPPERX_UINPUT_HELPER_SOCKET or XDG_RUNTIME_DIR must be set".to_string())?;
    Ok(PathBuf::from(runtime_dir)
        .join("pepper-x")
        .join("uinput-helper.sock"))
}

// ---------------------------------------------------------------------------
// XKB keymap → character mapping (direct + AltGr + dead keys)
// ---------------------------------------------------------------------------

fn build_char_mapper_from_env() -> Result<CharMapper, String> {
    let layout_raw = std::env::var("PEPPERX_XKB_LAYOUT").unwrap_or_else(|_| detect_layout());
    let variant_env = std::env::var("PEPPERX_XKB_VARIANT").unwrap_or_default();
    let (layout, variant) = split_layout_variant(&layout_raw, &variant_env);
    build_char_mapper(layout, variant)
}

fn build_char_mapper(layout: &str, variant: &str) -> Result<CharMapper, String> {
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);

    let keymap = xkb::Keymap::new_from_names(
        &context,
        "", // rules (default)
        "", // model (default)
        layout,
        variant,
        None, // options
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .ok_or_else(|| {
        format!("failed to compile XKB keymap for layout '{layout}', variant '{variant}'")
    })?;

    let shift_idx = keymap.mod_get_index(xkb::MOD_NAME_SHIFT);
    let level3_idx = keymap.mod_get_index(xkb::MOD_NAME_ISO_LEVEL3_SHIFT);

    let shift_mask = mod_bit(shift_idx);
    let level3_mask = mod_bit(level3_idx);

    let mod_combos: &[(u32, bool, bool)] = &[
        (0, false, false),
        (shift_mask, true, false),
        (level3_mask, false, true),
        (shift_mask | level3_mask, true, true),
    ];

    let mut state = xkb::State::new(&keymap);
    let mut map: HashMap<char, Vec<KeyChord>> = HashMap::new();
    // Base keysyms that can be typed with a single chord (for dead-key compose).
    let mut base_keysyms: Vec<(xkb::Keysym, KeyChord)> = Vec::new();
    let mut dead_keys: Vec<(xkb::Keysym, KeyChord)> = Vec::new();

    let min_keycode = keymap.min_keycode().raw();
    let max_keycode = keymap.max_keycode().raw();

    for &(mods, shift, alt_gr) in mod_combos {
        if (shift && shift_mask == 0) || (alt_gr && level3_mask == 0) {
            continue;
        }

        state.update_mask(mods, 0, 0, 0, 0, 0);

        for raw_kc in min_keycode..=max_keycode {
            let xkb_keycode = xkb::Keycode::new(raw_kc);
            let evdev_code = match raw_kc.checked_sub(XKB_EVDEV_OFFSET) {
                Some(code) if code > 0 && code <= u16::MAX as u32 => code as u16,
                _ => continue,
            };
            let chord = KeyChord {
                keycode: KeyCode::new(evdev_code),
                shift,
                alt_gr,
            };

            let sym = state.key_get_one_sym(xkb_keycode);
            if sym.raw() == 0 {
                continue;
            }

            let name = xkb::keysym_get_name(sym);
            if name.starts_with("dead_") {
                // Prefer unshifted dead keys when possible.
                if !dead_keys.iter().any(|(s, _)| *s == sym) {
                    dead_keys.push((sym, chord));
                }
                continue;
            }

            let utf32 = state.key_get_utf32(xkb_keycode);
            if utf32 == 0 || utf32 > 0x10FFFF {
                continue;
            }
            let Some(ch) = char::from_u32(utf32) else {
                continue;
            };
            if ch.is_control() && ch != '\n' && ch != '\t' {
                continue;
            }

            // Prefer shorter / simpler chords (no AltGr over AltGr, no shift over shift).
            let seq = vec![chord];
            insert_preferred_sequence(&mut map, ch, seq);

            if !base_keysyms.iter().any(|(s, _)| *s == sym) {
                base_keysyms.push((sym, chord));
            }
        }
    }

    // Compose dead-key sequences (e.g. dead_circumflex + e → ê on French AZERTY).
    let compose_added = expand_dead_keys(&context, &dead_keys, &base_keysyms, &mut map);

    // Ensure space, enter, tab are mapped even if the keymap is weird.
    map.entry(' ').or_insert_with(|| {
        vec![KeyChord {
            keycode: KeyCode::KEY_SPACE,
            shift: false,
            alt_gr: false,
        }]
    });
    map.entry('\n').or_insert_with(|| {
        vec![KeyChord {
            keycode: KeyCode::KEY_ENTER,
            shift: false,
            alt_gr: false,
        }]
    });
    map.entry('\t').or_insert_with(|| {
        vec![KeyChord {
            keycode: KeyCode::KEY_TAB,
            shift: false,
            alt_gr: false,
        }]
    });

    let hex_digits = build_hex_digit_map(&map);

    eprintln!(
        "[Pepper X uinput] XKB layout '{layout}' variant '{variant}' loaded, {} characters mapped ({} dead-key combos, {} hex digits)",
        map.len(),
        compose_added,
        hex_digits.len()
    );

    Ok(CharMapper { map, hex_digits })
}

fn mod_bit(index: xkb::ModIndex) -> u32 {
    if index == xkb::MOD_INVALID || index >= 31 {
        0
    } else {
        1u32 << index
    }
}

fn split_layout_variant<'a>(layout_raw: &'a str, variant_env: &'a str) -> (&'a str, &'a str) {
    if !variant_env.is_empty() {
        return (layout_raw, variant_env);
    }
    // gsettings may report "fr+mac" as a single token.
    if let Some((layout, variant)) = layout_raw.split_once('+') {
        (layout, variant)
    } else {
        (layout_raw, "")
    }
}

fn insert_preferred_sequence(map: &mut HashMap<char, Vec<KeyChord>>, ch: char, seq: Vec<KeyChord>) {
    match map.get(&ch) {
        None => {
            map.insert(ch, seq);
        }
        Some(existing) => {
            if sequence_rank(&seq) < sequence_rank(existing) {
                map.insert(ch, seq);
            }
        }
    }
}

/// Lower is better: fewer chords, fewer modifiers.
fn sequence_rank(seq: &[KeyChord]) -> (usize, usize) {
    let mod_cost: usize = seq
        .iter()
        .map(|c| usize::from(c.shift) + usize::from(c.alt_gr))
        .sum();
    (seq.len(), mod_cost)
}

fn expand_dead_keys(
    context: &xkb::Context,
    dead_keys: &[(xkb::Keysym, KeyChord)],
    base_keysyms: &[(xkb::Keysym, KeyChord)],
    map: &mut HashMap<char, Vec<KeyChord>>,
) -> usize {
    if dead_keys.is_empty() || base_keysyms.is_empty() {
        return 0;
    }

    let locale = compose_locale();
    let table = match xkb::compose::Table::new_from_locale(
        context,
        OsStr::new(&locale),
        xkb::compose::COMPILE_NO_FLAGS,
    ) {
        Ok(t) => t,
        Err(()) => {
            eprintln!(
                "[Pepper X uinput] compose table unavailable for locale '{locale}'; dead keys limited"
            );
            return 0;
        }
    };

    let mut added = 0usize;
    for &(dead_sym, dead_chord) in dead_keys {
        for &(base_sym, base_chord) in base_keysyms {
            let mut compose = xkb::compose::State::new(&table, xkb::compose::STATE_NO_FLAGS);
            compose.feed(dead_sym);
            if compose.status() != xkb::compose::Status::Composing {
                continue;
            }
            compose.feed(base_sym);
            if compose.status() != xkb::compose::Status::Composed {
                continue;
            }
            let Some(utf8) = compose.utf8() else {
                continue;
            };
            let mut chars = utf8.chars();
            let Some(ch) = chars.next() else {
                continue;
            };
            if chars.next().is_some() {
                // Multi-codepoint compose result — skip (rare).
                continue;
            }
            if ch.is_control() && ch != '\n' && ch != '\t' {
                continue;
            }

            let seq = vec![dead_chord, base_chord];
            let replace = match map.get(&ch) {
                None => true,
                Some(existing) => sequence_rank(&seq) < sequence_rank(existing),
            };
            if replace {
                map.insert(ch, seq);
                added += 1;
            }
        }
    }
    added
}

fn compose_locale() -> String {
    std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_CTYPE"))
        .or_else(|_| std::env::var("LANG"))
        .ok()
        .filter(|s| !s.is_empty() && s != "C" && !s.starts_with("C."))
        .unwrap_or_else(|| "en_US.UTF-8".into())
}

fn build_hex_digit_map(map: &HashMap<char, Vec<KeyChord>>) -> HashMap<char, KeyChord> {
    let mut hex = HashMap::new();
    for ch in "0123456789abcdef".chars() {
        if let Some(seq) = map.get(&ch) {
            if seq.len() == 1 {
                hex.insert(ch, seq[0]);
            }
        }
    }
    // Hard fallbacks for ASCII hex if layout map is incomplete.
    const FALLBACKS: &[(char, KeyCode, bool)] = &[
        ('0', KeyCode::KEY_0, false),
        ('1', KeyCode::KEY_1, false),
        ('2', KeyCode::KEY_2, false),
        ('3', KeyCode::KEY_3, false),
        ('4', KeyCode::KEY_4, false),
        ('5', KeyCode::KEY_5, false),
        ('6', KeyCode::KEY_6, false),
        ('7', KeyCode::KEY_7, false),
        ('8', KeyCode::KEY_8, false),
        ('9', KeyCode::KEY_9, false),
        ('a', KeyCode::KEY_A, false),
        ('b', KeyCode::KEY_B, false),
        ('c', KeyCode::KEY_C, false),
        ('d', KeyCode::KEY_D, false),
        ('e', KeyCode::KEY_E, false),
        ('f', KeyCode::KEY_F, false),
    ];
    for &(ch, keycode, shift) in FALLBACKS {
        hex.entry(ch).or_insert(KeyChord {
            keycode,
            shift,
            alt_gr: false,
        });
    }
    hex
}

fn resolve_stroke(mapper: &CharMapper, ch: char) -> CharStroke {
    if let Some(seq) = mapper.map.get(&ch) {
        return CharStroke::Chords(seq.clone());
    }
    // Any remaining Unicode: Ctrl+Shift+U hex entry.
    CharStroke::UnicodeHex(ch as u32)
}

fn detect_layout() -> String {
    // Try reading from gsettings
    if let Ok(output) = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.input-sources", "sources"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Format: [('xkb', 'us'), ('xkb', 'fr+mac')]
        if let Some(start) = stdout.find("'xkb', '") {
            let rest = &stdout[start + 8..];
            if let Some(end) = rest.find('\'') {
                let layout = &rest[..end];
                if !layout.is_empty() {
                    eprintln!("[Pepper X uinput] detected layout from gsettings: {layout}");
                    return layout.to_string();
                }
            }
        }
    }

    // Try /etc/default/keyboard
    if let Ok(content) = std::fs::read_to_string("/etc/default/keyboard") {
        for line in content.lines() {
            if let Some(layout) = line.strip_prefix("XKBLAYOUT=") {
                let layout = layout.trim_matches('"').trim();
                if !layout.is_empty() {
                    let first = layout.split(',').next().unwrap_or(layout);
                    eprintln!(
                        "[Pepper X uinput] detected layout from /etc/default/keyboard: {first}"
                    );
                    return first.to_string();
                }
            }
        }
    }

    eprintln!("[Pepper X uinput] no layout detected, defaulting to 'us'");
    "us".to_string()
}

// ---------------------------------------------------------------------------
// Virtual keyboard
// ---------------------------------------------------------------------------

fn create_virtual_keyboard(mapper: &CharMapper) -> Result<VirtualDevice, String> {
    let mut keys = AttributeSet::<KeyCode>::new();

    for seq in mapper.map.values() {
        for chord in seq {
            keys.insert(chord.keycode);
        }
    }
    for chord in mapper.hex_digits.values() {
        keys.insert(chord.keycode);
    }

    // Modifiers + Unicode entry helpers
    keys.insert(KeyCode::KEY_LEFTSHIFT);
    keys.insert(KeyCode::KEY_RIGHTSHIFT);
    keys.insert(KeyCode::KEY_LEFTCTRL);
    keys.insert(KeyCode::KEY_RIGHTALT); // AltGr / ISO_Level3_Shift
    keys.insert(KeyCode::KEY_U);
    keys.insert(KeyCode::KEY_SPACE);
    keys.insert(KeyCode::KEY_ENTER);
    keys.insert(KeyCode::KEY_TAB);

    // Full alphanumeric set so Unicode hex works even if layout map was sparse.
    for code in [
        KeyCode::KEY_0,
        KeyCode::KEY_1,
        KeyCode::KEY_2,
        KeyCode::KEY_3,
        KeyCode::KEY_4,
        KeyCode::KEY_5,
        KeyCode::KEY_6,
        KeyCode::KEY_7,
        KeyCode::KEY_8,
        KeyCode::KEY_9,
        KeyCode::KEY_A,
        KeyCode::KEY_B,
        KeyCode::KEY_C,
        KeyCode::KEY_D,
        KeyCode::KEY_E,
        KeyCode::KEY_F,
    ] {
        keys.insert(code);
    }

    let device = VirtualDevice::builder()
        .map_err(|error| format!("failed to create uinput builder: {error}"))?
        .name("Pepper X virtual keyboard")
        .with_keys(&keys)
        .map_err(|error| format!("failed to configure keyboard capabilities: {error}"))?
        .build()
        .map_err(|error| format!("failed to create Pepper X uinput device: {error}"))?;

    std::thread::sleep(STARTUP_DELAY);
    Ok(device)
}

// ---------------------------------------------------------------------------
// Connection handling
// ---------------------------------------------------------------------------

fn handle_connection(
    mut stream: UnixStream,
    device: &mut VirtualDevice,
    mapper: &CharMapper,
) -> Result<(), String> {
    let request: UinputInsertRequest = serde_json::from_reader(BufReader::new(
        stream
            .try_clone()
            .map_err(|error| format!("failed to clone helper stream: {error}"))?,
    ))
    .map_err(|error| format!("failed to parse helper request: {error}"))?;

    let response = match type_text(device, &request.text, mapper) {
        Ok(()) => UinputInsertResponse {
            ok: true,
            error: None,
        },
        Err(error) => UinputInsertResponse {
            ok: false,
            error: Some(error),
        },
    };

    serde_json::to_writer(&mut stream, &response)
        .map_err(|error| format!("failed to encode helper response: {error}"))?;
    stream
        .write_all(b"\n")
        .map_err(|error| format!("failed to finish helper response: {error}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Text emission
// ---------------------------------------------------------------------------

fn type_text(device: &mut VirtualDevice, text: &str, mapper: &CharMapper) -> Result<(), String> {
    // Resolve every character first so we never partially inject then fail mid-string
    // for a reason other than device I/O.
    let strokes: Vec<(char, CharStroke)> = text
        .chars()
        .map(|ch| (ch, resolve_stroke(mapper, ch)))
        .collect();

    let mut unicode_fallbacks = 0u32;
    for (ch, stroke) in &strokes {
        match stroke {
            CharStroke::Chords(seq) => {
                for (i, chord) in seq.iter().enumerate() {
                    emit_chord(device, *chord)?;
                    if i + 1 < seq.len() {
                        // Dead-key sequences need a beat for the client compose state.
                        std::thread::sleep(DEAD_KEY_DELAY);
                    } else {
                        std::thread::sleep(INTER_KEY_DELAY);
                    }
                }
            }
            CharStroke::UnicodeHex(cp) => {
                unicode_fallbacks += 1;
                eprintln!(
                    "[Pepper X uinput] unicode hex fallback for {:?} (U+{cp:04X})",
                    ch
                );
                emit_unicode_hex(device, *cp, mapper)?;
                std::thread::sleep(INTER_KEY_DELAY);
            }
        }
    }

    if unicode_fallbacks > 0 {
        eprintln!(
            "[Pepper X uinput] typed {} chars ({} via unicode hex)",
            strokes.len(),
            unicode_fallbacks
        );
    }

    Ok(())
}

fn emit_chord(device: &mut VirtualDevice, chord: KeyChord) -> Result<(), String> {
    if chord.alt_gr {
        emit_key(device, KeyCode::KEY_RIGHTALT, 1)?;
    }
    if chord.shift {
        emit_key(device, KeyCode::KEY_LEFTSHIFT, 1)?;
    }

    emit_key(device, chord.keycode, 1)?;
    std::thread::sleep(KEY_HOLD_DELAY);
    emit_key(device, chord.keycode, 0)?;

    if chord.shift {
        emit_key(device, KeyCode::KEY_LEFTSHIFT, 0)?;
    }
    if chord.alt_gr {
        emit_key(device, KeyCode::KEY_RIGHTALT, 0)?;
    }

    Ok(())
}

/// GNOME/IBus/GTK Unicode entry: Ctrl+Shift+U, hex digits, Space to commit.
fn emit_unicode_hex(
    device: &mut VirtualDevice,
    codepoint: u32,
    mapper: &CharMapper,
) -> Result<(), String> {
    // Enter unicode mode
    emit_key(device, KeyCode::KEY_LEFTCTRL, 1)?;
    emit_key(device, KeyCode::KEY_LEFTSHIFT, 1)?;
    emit_key(device, KeyCode::KEY_U, 1)?;
    std::thread::sleep(KEY_HOLD_DELAY);
    emit_key(device, KeyCode::KEY_U, 0)?;
    emit_key(device, KeyCode::KEY_LEFTSHIFT, 0)?;
    emit_key(device, KeyCode::KEY_LEFTCTRL, 0)?;
    std::thread::sleep(UNICODE_MODE_DELAY);

    let hex = format!("{codepoint:x}");
    for digit in hex.chars() {
        let chord = mapper.hex_digits.get(&digit).copied().ok_or_else(|| {
            format!("internal error: missing hex digit mapping for {digit:?}")
        })?;
        emit_chord(device, chord)?;
        std::thread::sleep(INTER_KEY_DELAY);
    }

    // Commit
    emit_key(device, KeyCode::KEY_SPACE, 1)?;
    std::thread::sleep(KEY_HOLD_DELAY);
    emit_key(device, KeyCode::KEY_SPACE, 0)?;
    std::thread::sleep(UNICODE_MODE_DELAY);

    Ok(())
}

fn emit_key(device: &mut VirtualDevice, key: KeyCode, value: i32) -> Result<(), String> {
    let events = [
        InputEvent::new(EventType::KEY.0, key.0, value),
        InputEvent::new(
            EventType::SYNCHRONIZATION.0,
            SynchronizationCode::SYN_REPORT.0,
            0,
        ),
    ];
    device
        .emit(&events)
        .map_err(|error| format!("failed to emit uinput key event: {error}"))
}

// ---------------------------------------------------------------------------
// Tests (layout map only — no /dev/uinput required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn mapper_for_layout(layout: &str) -> CharMapper {
        build_char_mapper(layout, "").expect("keymap should compile")
    }

    #[test]
    fn french_layout_maps_circumflex_e() {
        let mapper = mapper_for_layout("fr");
        // ê must be reachable (dead key ^ + e on basic AZERTY).
        let stroke = resolve_stroke(&mapper, 'ê');
        match stroke {
            CharStroke::Chords(seq) => {
                assert!(
                    seq.len() >= 1,
                    "ê should map to at least one chord, got {seq:?}"
                );
                // Prefer real layout sequence over unicode when possible.
                assert!(
                    seq.len() <= 2,
                    "ê sequence should be short (direct or dead+base), got len {}",
                    seq.len()
                );
            }
            CharStroke::UnicodeHex(cp) => {
                panic!("ê should be layout-mappable on fr, got unicode fallback U+{cp:04X}");
            }
        }
    }

    #[test]
    fn french_layout_maps_common_accents() {
        let mapper = mapper_for_layout("fr");
        for ch in ['é', 'è', 'à', 'ù', 'ç', 'â', 'ê', 'î', 'ô', 'û', 'ë', 'ï', 'ü'] {
            match resolve_stroke(&mapper, ch) {
                CharStroke::Chords(seq) => assert!(!seq.is_empty(), "{ch} empty sequence"),
                CharStroke::UnicodeHex(_) => {
                    // Dead-key expand may miss some depending on compose locale;
                    // unicode fallback still types them.
                }
            }
            // All must resolve without error.
            let _ = resolve_stroke(&mapper, ch);
        }
        // Direct AZERTY base letters with accent keys:
        assert!(mapper.map.contains_key(&'é'), "é should be direct on fr");
        assert!(mapper.map.contains_key(&'è'), "è should be direct on fr");
        assert!(mapper.map.contains_key(&'à'), "à should be direct on fr");
        assert!(mapper.map.contains_key(&'ç'), "ç should be direct on fr");
        assert!(
            mapper.map.contains_key(&'ê'),
            "ê should be in map via dead keys on fr (got {} chars)",
            mapper.map.len()
        );
    }

    #[test]
    fn any_unicode_resolves_via_hex_fallback() {
        let mapper = mapper_for_layout("us");
        for ch in ['😀', '中', 'ß', 'œ', '€'] {
            match resolve_stroke(&mapper, ch) {
                CharStroke::Chords(_) => {}
                CharStroke::UnicodeHex(cp) => assert_eq!(cp, ch as u32),
            }
        }
    }

    #[test]
    fn split_layout_variant_handles_gsettings_plus_form() {
        assert_eq!(split_layout_variant("fr+mac", ""), ("fr", "mac"));
        assert_eq!(split_layout_variant("fr", "oss"), ("fr", "oss"));
        assert_eq!(split_layout_variant("us", ""), ("us", ""));
    }

    #[test]
    fn apostrophe_phrase_fully_resolves_on_fr() {
        let mapper = mapper_for_layout("fr");
        let text = "C'est peut-être bon";
        for ch in text.chars() {
            let _ = resolve_stroke(&mapper, ch);
            // Must not panic; layout or unicode covers everything.
            match resolve_stroke(&mapper, ch) {
                CharStroke::Chords(seq) => assert!(!seq.is_empty(), "empty for {ch:?}"),
                CharStroke::UnicodeHex(_) => {}
            }
        }
        assert!(
            matches!(
                resolve_stroke(&mapper, 'ê'),
                CharStroke::Chords(_)
            ),
            "ê in sample phrase must use layout chords on fr"
        );
    }
}
