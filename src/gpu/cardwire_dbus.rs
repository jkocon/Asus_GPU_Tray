//! cardwired over D-Bus (org.opengamingcollective.cardwire, cardwire 0.12).

use std::collections::HashMap;
use std::time::Duration;

use zbus::blocking::{proxy, Connection};
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use super::cardwire::{mode_name, order_modes, Cardwire, CwDevice, CwDevices};

pub const SERVICE: &str = "org.opengamingcollective.cardwire";
pub const ROOT: &str = "/org/opengamingcollective/cardwire";
const GPU_IFACE: &str = "org.opengamingcollective.cardwire.Gpu";
const MODE_IFACE: &str = "org.opengamingcollective.cardwire.Mode";
const DEBUG_IFACE: &str = "org.opengamingcollective.cardwire.Debug";
/// Like the 5 s limit on the commands the Python tray ran.
const TIMEOUT: Duration = Duration::from_secs(5);

/// GetDevice: name, pci, render, card, default, discrete, virtual, available, vendor, driver,
/// nvidia, nvidia_minor
type Device = (String, String, u32, u32, bool, bool, bool, bool, String, String, bool, String);
type Managed = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

pub struct DbusCardwire {
    conn: Option<Connection>,
}

impl DbusCardwire {
    pub fn connect() -> Self {
        let conn = zbus::blocking::connection::Builder::system().and_then(|b| b.method_timeout(TIMEOUT).build()).ok();
        DbusCardwire { conn }
    }

    fn proxy<'a>(&'a self, path: &'a str, iface: &'a str) -> Option<proxy::Proxy<'a>> {
        proxy::Builder::new(self.conn.as_ref()?)
            .destination(SERVICE)
            .ok()?
            .path(path)
            .ok()?
            .interface(iface)
            .ok()?
            .cache_properties(CacheProperties::No)
            .build()
            .ok()
    }
}

impl Cardwire for DbusCardwire {
    fn devices(&self) -> Option<CwDevices> {
        let manager = self.proxy(ROOT, "org.freedesktop.DBus.ObjectManager")?;
        let objects: Managed = manager.call("GetManagedObjects", &()).ok()?;
        let mut out = CwDevices::new();
        for (path, ifaces) in objects {
            let Some(props) = ifaces.get(GPU_IFACE) else { continue };
            let blocked = props.get("Block").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
            let Some(gpu) = self.proxy(path.as_str(), GPU_IFACE) else { continue };
            let Ok(d) = gpu.call::<_, _, Device>("GetDevice", &()) else { continue };
            out.insert(d.1, CwDevice { name: Some(d.0), vendor: Some(d.8), driver: d.9, blocked, discrete: d.5 });
        }
        Some(out)
    }

    fn mode(&self) -> (String, Vec<String>) {
        let Some(p) = self.proxy(ROOT, MODE_IFACE) else { return Default::default() };
        let mode = p.get_property::<u32>("Mode").map(mode_name).unwrap_or_default();
        let modes = p.call::<_, _, Vec<u32>>("AvailableModes", &()).unwrap_or_default();
        (mode, order_modes(modes.into_iter().map(mode_name).collect()))
    }

    fn refresh_gpu(&self) {
        if let Some(p) = self.proxy(ROOT, DEBUG_IFACE) {
            let _ = p.call::<_, _, ()>("RefreshGpu", &());
        }
    }
}
