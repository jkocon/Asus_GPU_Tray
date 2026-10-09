//! Translations through gettext, with the msgids of asus_gpu_tray.py: po/asus-gpu-tray.pot and
//! po/pl.po are reused as they are. Without init() (tests, dump) every text stays English.

use std::path::Path;

pub const DOMAIN: &str = "asus-gpu-tray";

/// Use <dir>/<lang>/LC_MESSAGES/asus-gpu-tray.mo; the language follows LANGUAGE / LC_ALL /
/// LC_MESSAGES / LANG. Call it first in main, before any other thread starts.
pub fn init(locale_dir: &Path) {
    use gettextrs::{bind_textdomain_codeset, bindtextdomain, setlocale, textdomain, LocaleCategory};
    // SAFETY: setlocale is not thread-safe; this runs before the program starts any thread.
    unsafe { setlocale(LocaleCategory::LcAll, "") };
    let _ = bindtextdomain(DOMAIN, locale_dir);
    let _ = bind_textdomain_codeset(DOMAIN, "UTF-8");
    let _ = textdomain(DOMAIN);
}

/// Translated text.
pub fn tr(msgid: &str) -> String {
    gettextrs::gettext(msgid)
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
