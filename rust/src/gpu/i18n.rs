//! Translations. For now English only; phase 2 hooks gettext in here, with the same msgids as
//! asus_gpu_tray.py, so po/asus-gpu-tray.pot and po/pl.po are reused as they are.

/// Translated text (identity until gettext is wired up).
pub fn tr(msgid: &str) -> String {
    msgid.to_string()
}

/// Python's str.format with named fields: fill("{igpu} only", &[("igpu", "Radeon")]).
pub fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (key, value) in args {
        out = out.replace(&format!("{{{key}}}"), value);
    }
    out
}

/// tr + fill in one call.
pub fn trf(msgid: &str, args: &[(&str, &str)]) -> String {
    fill(&tr(msgid), args)
}

/// First letter upper case (translations may start with a lower-case word).
pub fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
