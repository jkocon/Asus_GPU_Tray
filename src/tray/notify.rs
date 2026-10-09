//! Desktop notifications (org.freedesktop.Notifications), what QSystemTrayIcon.showMessage did.

use std::collections::HashMap;

use zbus::blocking::Connection;
use zbus::zvariant::Value;

pub struct Notifier {
    conn: Option<Connection>,
}

/// The body may be shown as markup; the texts contain process and device names.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

impl Notifier {
    pub fn new() -> Self {
        Notifier { conn: Connection::session().ok() }
    }

    pub fn show(&self, summary: &str, body: &str, timeout_ms: i32, critical: bool) {
        let Some(conn) = &self.conn else { return };
        let mut hints: HashMap<&str, Value> = HashMap::new();
        hints.insert("desktop-entry", Value::from("asus-gpu-tray"));
        if critical {
            hints.insert("urgency", Value::U8(2));
        }
        let actions: Vec<&str> = Vec::new();
        let _ = conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &("Asus GPU Tray", 0u32, "asus-gpu-tray", summary, escape(body), actions, hints, timeout_ms),
        );
    }
}
