//! Every text passed to tr()/trf() in the sources has a Polish translation in po/pl.po (the msgids
//! are shared with asus_gpu_tray.py).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// The value of a Rust string literal body: escapes and `\` line continuations.
fn unescape_rust(lit: &str) -> String {
    let mut out = String::new();
    let mut chars = lit.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('\n') => {
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
            }
            Some(other) => panic!("unexpected escape \\{other}"),
            None => {}
        }
    }
    out
}

/// String literals right after `tr(` or `trf(`.
fn msgids_in(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for start in ["tr(", "trf("] {
        let mut rest = src;
        while let Some(i) = rest.find(start) {
            let before = rest[..i].chars().last();
            rest = &rest[i + start.len()..];
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue; // e.g. str(
            }
            let trimmed = rest.trim_start();
            let Some(body) = trimmed.strip_prefix('"') else { continue }; // tr(variable)
            let mut end = None;
            let mut escaped = false;
            for (j, c) in body.char_indices() {
                match c {
                    '\\' if !escaped => escaped = true,
                    '"' if !escaped => {
                        end = Some(j);
                        break;
                    }
                    _ => escaped = false,
                }
            }
            out.push(unescape_rust(&body[..end.expect("closed literal")]));
        }
    }
    out
}

fn unescape_po(s: &str) -> String {
    s.replace("\\n", "\n").replace("\\\"", "\"").replace("\\\\", "\\")
}

/// msgid -> msgstr, joining continuation lines.
fn po_entries(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let (mut id, mut msg, mut in_str) = (String::new(), String::new(), false);
    let quoted =
        |l: &str| unescape_po(l.trim().trim_start_matches("msgid ").trim_start_matches("msgstr ").trim_matches('"'));
    for line in text.lines().chain(std::iter::once("")) {
        if line.starts_with("msgid ") {
            if !id.is_empty() {
                map.insert(std::mem::take(&mut id), std::mem::take(&mut msg));
            }
            (id, msg, in_str) = (quoted(line), String::new(), false);
        } else if line.starts_with("msgstr ") {
            (msg, in_str) = (quoted(line), true);
        } else if line.starts_with('"') {
            if in_str {
                msg += &quoted(line)
            } else {
                id += &quoted(line)
            }
        }
    }
    if !id.is_empty() {
        map.insert(id, msg);
    }
    map
}

fn sources(dir: &Path, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(fs::read_to_string(&path).unwrap());
        }
    }
}

#[test]
fn every_text_has_a_polish_translation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let po = po_entries(&fs::read_to_string(root.join("po/pl.po")).unwrap());
    let mut srcs = Vec::new();
    sources(&root.join("src"), &mut srcs);
    let used: Vec<String> = srcs.iter().flat_map(|s| msgids_in(s)).collect();
    assert!(used.len() > 90, "found only {} texts - is the scan broken?", used.len());
    let missing: Vec<&String> = used.iter().filter(|m| po.get(*m).is_none_or(|t| t.is_empty())).collect();
    assert!(missing.is_empty(), "not translated in po/pl.po: {missing:#?}");
}

#[test]
fn scanner() {
    assert_eq!(msgids_in(r#"tr("a \"b\"\n") + &trf("x {y}", &[]) + str("no") + tr(var)"#), ["a \"b\"\n", "x {y}"]);
    assert_eq!(unescape_rust("one \\\n        two"), "one two");
}
