#!/usr/bin/env python3
"""Asus GPU Tray: a system tray icon that shows which GPU is rendering and switches GPU modes.

- Live GPU access modes (Integrated / Hybrid / Smart) through cardwire - no reboot, no logout.
- Hardware modes on ASUS laptops (built-in dGPU / XG Mobile / MUX) through asus-armoury,
  applied during the next boot before the NVIDIA driver loads.
- supergfxd is used as a fallback when cardwire is not installed.
- Without any of them it works as a read-only GPU viewer.

  asus_gpu_tray.py          system tray icon
  asus_gpu_tray.py --dump   print the detected state and exit"""

import fcntl
import json
import os
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path

from PyQt6.QtCore import QRectF, Qt, QTimer
from PyQt6.QtGui import QAction, QActionGroup, QColor, QFont, QFontMetrics, QIcon, QPainter, QPixmap
from PyQt6.QtWidgets import QApplication, QMenu, QMessageBox, QSystemTrayIcon

APP_NAME = "Asus GPU Tray"
ATTR = Path("/sys/class/firmware-attributes/asus-armoury/attributes")
PCI = Path("/sys/bus/pci/devices")
PCI_IDS = "/usr/share/hwdata/pci.ids"
# Reboot backend (switch at boot, before the NVIDIA driver loads); the polkit rule allows only these modes.
REBOOT_UNIT = Path("/etc/systemd/system/asus-gpu-switch@.service")
REBOOT_MODES = ("Integrated", "Hybrid", "AsusEgpu", "AsusMuxDgpu")
# Experimental live hardware switch (built-in dGPU <-> XG Mobile) without a reboot.
LIVE_UNIT = Path("/etc/systemd/system/asus-gpu-live@.service")
LIVE_RESULT = Path("/var/lib/asus-gpu-tray/live-result")
POLL_MS = 3000
VENDORS = {"10de": "NVIDIA", "1002": "AMD", "8086": "Intel"}
KIND_LABEL = {"igpu": "iGPU", "dgpu": "dGPU", "egpu": "eGPU"}
CARDWIRE_MODES = ("integrated", "hybrid", "smart")  # order in the menu
REFRESH_EVERY_S = 60
_last_cw_refresh = 0.0


@dataclass(frozen=True)
class Gpu:
    addr: str
    vendor: str  # "NVIDIA" / "AMD" / "Intel" / raw vendor ID
    name: str
    driver: str
    kind: str  # igpu / dgpu / egpu
    power: str  # runtime_status from sysfs
    blocked: bool  # hidden from new apps by cardwire


@dataclass(frozen=True)
class GpuState:
    gpus: tuple[Gpu, ...]
    cardwire: bool
    cw_mode: str  # lower case, e.g. "hybrid"
    cw_modes: tuple[str, ...]
    supergfx: bool
    mode: str  # supergfxd mode
    supported: tuple[str, ...]
    dgpu_vendor: str
    pending: str
    pending_action: str
    asus_egpu: bool  # egpu_connected attribute exists (ASUS XG Mobile)
    egpu_connected: bool
    hw_mode: str  # "Hybrid" (built-in dGPU) / "AsusEgpu" / "AsusMuxDgpu" / "" when not ASUS
    has_mux: bool
    hw_pending: str  # hardware mode scheduled for the next boot
    reboot_backend: bool
    live_backend: bool

    @property
    def igpu(self) -> Gpu | None:
        return next((g for g in self.gpus if g.kind == "igpu"), None)

    @property
    def dgpu(self) -> Gpu | None:
        return next((g for g in self.gpus if g.kind == "dgpu"), None)

    @property
    def egpu(self) -> Gpu | None:
        return next((g for g in self.gpus if g.kind == "egpu"), None)


def run(*cmd: str) -> str:
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=5).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return ""


def read(path: Path) -> str:
    try:
        return path.read_text().strip()
    except OSError:
        return ""


def read_attr(name: str) -> str:
    return read(ATTR / name / "current_value")


@lru_cache
def pci_ids_name(vendor: str, device: str) -> str:
    try:
        in_vendor = False
        with open(PCI_IDS, encoding="utf-8", errors="replace") as f:
            for line in f:
                if line.startswith("#") or not line.strip():
                    continue  # comments also appear inside a vendor block
                if not line.startswith("\t"):
                    in_vendor = line.startswith(vendor)
                elif in_vendor and line.startswith(f"\t{device}"):
                    return line.split(None, 1)[1].strip()
    except OSError:
        pass
    return ""


def short_name(vendor: str, device: str) -> str:
    # "GA104M [GeForce RTX 3070 Mobile / Max-Q]" -> "RTX 3070"
    name = pci_ids_name(vendor, device) or f"{vendor}:{device}"
    if "[" in name:
        name = name[name.index("[") + 1 : name.rindex("]")]
    name = name.split(" / ")[0]
    return name.replace("GeForce ", "").replace(" Mobile", "").replace(" Max-Q", "").strip()


def is_integrated(dev: Path, vendor: str) -> bool:
    domain, bus, _ = dev.name.split(":", 2)
    if vendor == "8086":
        return bus == "00"  # Intel iGPU lives on bus 0, Arc cards sit behind a PCIe bridge
    if vendor == "1002":
        # AMD APU: the CPU's USB controllers share the GPU's bus; a dGPU only has HDMI audio next to it
        for sib in PCI.glob(f"{domain}:{bus}:*"):
            if read(sib / "class").startswith("0x0c03"):
                return True
    return False


def is_external(dev: Path) -> bool:
    """Thunderbolt/USB4: the kernel marks ports that lead outside the machine."""
    if read(dev / "removable") == "removable":
        return True
    real = dev.resolve()
    return any(read(p / "external_facing") == "1" for p in real.parents if p.name.count(":") == 2)


def cardwire_devices() -> dict[str, dict] | None:
    """PCI address -> cardwire's device info, or None when cardwire is not available."""
    if not shutil.which("cardwire"):
        return None
    try:
        devices = json.loads(run("cardwire", "list", "--json") or "null")
        return {d["pci"]: d for d in devices.values()}
    except (ValueError, AttributeError, KeyError, TypeError):
        return None


def cardwire_short_name(name: str) -> str:
    # "NVIDIA GeForce RTX 3070 Laptop GPU" -> "RTX 3070"
    for word in ("NVIDIA ", "GeForce ", "AMD ", "Intel(R) ", "Intel ", " Laptop GPU", " Mobile"):
        name = name.replace(word, "")
    return name.strip()


def detect_gpus(cw: dict[str, dict]) -> tuple[Gpu, ...]:
    # Only kernel-cached sysfs files - lspci reads config space and wakes a suspended dGPU.
    xg_active = read_attr("egpu_enable") == "1"
    gpus = []
    seen = set()
    for dev in sorted(PCI.iterdir()):
        if not read(dev / "class").startswith("0x03"):
            continue
        vendor = read(dev / "vendor")[2:]
        device = read(dev / "device")[2:]
        if is_integrated(dev, vendor):
            kind = "igpu"
        elif is_external(dev) or (xg_active and vendor == "10de"):
            kind = "egpu"  # XG Mobile takes over the same port as the built-in dGPU
        else:
            kind = "dgpu"
        driver = dev / "driver"
        gpus.append(
            Gpu(
                addr=dev.name,
                vendor=VENDORS.get(vendor, vendor),
                name=short_name(vendor, device),
                driver=driver.resolve().name if driver.exists() else "",
                kind=kind,
                power=read(dev / "power" / "runtime_status"),
                blocked=bool(cw.get(dev.name, {}).get("blocked")),
            )
        )
        seen.add(dev.name)
    # cardwire hides a blocked GPU's sysfs files from everyone, so take it from cardwire itself.
    for addr, d in sorted(cw.items()):
        if addr in seen or not d.get("blocked"):
            continue
        vendor = {"Nvidia": "NVIDIA"}.get(d.get("vendor", ""), d.get("vendor", "?"))
        if not d.get("discrete"):
            kind = "igpu"
        elif is_external(PCI / addr) or (xg_active and vendor == "NVIDIA"):
            kind = "egpu"
        else:
            kind = "dgpu"
        gpus.append(Gpu(addr, vendor, cardwire_short_name(d.get("name", addr)), d.get("driver", ""), kind, "", True))
    return tuple(sorted(gpus, key=lambda g: g.addr))


def parse_cardwire_get(out: str) -> tuple[str, tuple[str, ...]]:
    # "Current Mode: Hybrid\nAvailable Mode: integrated, hybrid, smart"
    mode, modes = "", ()
    for line in out.splitlines():
        key, _, value = line.partition(":")
        if key.strip() == "Current Mode":
            mode = value.strip().lower()
        elif key.strip() == "Available Mode":
            modes = tuple(m.strip().lower() for m in value.split(",") if m.strip())
    return mode, tuple(m for m in CARDWIRE_MODES if m in modes) + tuple(m for m in modes if m not in CARDWIRE_MODES)


def cardwire_missed_dgpu(cw: dict[str, dict]) -> bool:
    """cardwired can start before the NVIDIA driver is ready and then mistake the dGPU for an
    integrated one (the laptop looks like a desktop: only hybrid/manual modes)."""
    if not cw or any(d.get("discrete") for d in cw.values()):
        return False
    return any(g.kind != "igpu" and g.driver for g in detect_gpus(cw))


def read_state() -> GpuState:
    global _last_cw_refresh
    cw = cardwire_devices()
    if cw and cardwire_missed_dgpu(cw) and time.monotonic() - _last_cw_refresh > REFRESH_EVERY_S:
        _last_cw_refresh = time.monotonic()
        run("cardwire", "debug", "refresh-gpu")
        cw = cardwire_devices()
    cw_mode, cw_modes = parse_cardwire_get(run("cardwire", "get")) if cw is not None else ("", ())
    supergfx = shutil.which("supergfxctl") is not None
    mode = supported = dgpu_vendor = pending = action = ""
    if supergfx:
        mode = run("supergfxctl", "-g")
        supported = run("supergfxctl", "-s")
        dgpu_vendor = run("supergfxctl", "-V")
        pending = run("supergfxctl", "-P")
        action = run("supergfxctl", "-p")
    asus_egpu = (ATTR / "egpu_connected").exists()
    hw_mode = ""
    if asus_egpu:
        if read_attr("egpu_enable") == "1":
            hw_mode = "AsusEgpu"
        elif read_attr("gpu_mux_mode") == "0":
            hw_mode = "AsusMuxDgpu"
        else:
            hw_mode = "Hybrid"
    return GpuState(
        gpus=detect_gpus(cw or {}),
        cardwire=bool(cw_mode),
        cw_mode=cw_mode,
        cw_modes=cw_modes,
        supergfx=supergfx and bool(mode),
        mode=mode or "?",
        supported=tuple(m.strip() for m in supported.strip("[]").split(",") if m.strip()),
        dgpu_vendor=dgpu_vendor,
        pending="" if pending in ("", "None", "Unknown") else pending,
        pending_action="" if action in ("", "Nothing", "None") else action,
        asus_egpu=asus_egpu,
        egpu_connected=read_attr("egpu_connected") == "1",
        hw_mode=hw_mode,
        has_mux=(ATTR / "gpu_mux_mode").exists(),
        hw_pending=read(Path("/var/lib/asus-gpu-tray/pending")),
        reboot_backend=REBOOT_UNIT.exists() and asus_egpu,
        live_backend=LIVE_UNIT.exists() and asus_egpu,
    )


def dgpu_name(s: GpuState) -> str:
    if s.dgpu:
        return s.dgpu.name
    return f"built-in {s.dgpu_vendor}" if s.dgpu_vendor else "built-in dGPU"


def mode_label(mode: str, s: GpuState) -> str:
    """Label for a supergfxd / hardware mode."""
    ig = s.igpu.name if s.igpu else "iGPU"
    dg = dgpu_name(s)
    label = {
        "Integrated": f"{ig} only",
        "Hybrid": f"Hybrid ({ig} + {dg})",
        "AsusEgpu": "XG Mobile" + (f" ({s.egpu.name})" if s.egpu else ""),
        "AsusMuxDgpu": f"{dg} only (MUX)",
        "Vfio": f"VFIO ({dg} for virtual machines)",
        "NvidiaNoModeset": f"Hybrid without NVIDIA modeset ({ig} + {dg})",
    }.get(mode, mode)
    return label[0].upper() + label[1:]


def hw_label(mode: str, s: GpuState) -> str:
    """Label for a hardware mode switched with a reboot (ASUS)."""
    if mode == "Hybrid":
        return f"Built-in dGPU ({s.dgpu.name})" if s.dgpu else "Built-in dGPU"
    if mode == "AsusMuxDgpu":
        return f"{s.dgpu.name if s.dgpu else 'Built-in dGPU'} only (MUX)"
    return mode_label(mode, s)


def cw_label(mode: str, s: GpuState) -> str:
    ext = s.egpu or s.dgpu
    name = ext.name if ext else "dGPU"
    return {
        "integrated": f"Integrated – block {name}",
        "hybrid": "Hybrid – all GPUs available",
        "smart": f"Smart – {name} only for approved apps",
    }.get(mode, mode.capitalize())


def working_gpu(s: GpuState) -> Gpu | None:
    """The card currently rendering (shown by the icon)."""
    for g in (s.egpu, s.dgpu):
        if g and g.driver and not g.blocked and g.power == "active":
            return g
    if s.hw_mode == "AsusMuxDgpu" or s.mode == "AsusMuxDgpu":
        return s.dgpu or s.igpu  # with the MUX the dGPU drives the panel
    return s.igpu or (s.gpus[0] if s.gpus else None)


def state_label(g: Gpu) -> str:
    if g.blocked:
        return f"{g.power}, blocked" if g.power else "blocked"
    return g.power or "no runtime PM"


def describe(s: GpuState) -> str:
    g = working_gpu(s)
    if not g:
        return "No graphics card detected"
    others = [x for x in s.gpus if x is not g]
    text = f"{g.name} ({KIND_LABEL[g.kind]})"
    if others:
        text += " · " + ", ".join(f"{x.name} {state_label(x)}" for x in others)
    return text


def gpu_line(g: Gpu) -> str:
    return f"{KIND_LABEL[g.kind]}: {g.name} – {g.driver or 'no driver'}, {state_label(g)}"


def xg_line(s: GpuState) -> str:
    return f"XG Mobile: {'connected' if s.egpu_connected else 'not connected'}"


def dump(s: GpuState) -> None:
    for g in s.gpus:
        print(f"{g.addr}  {gpu_line(g)}  [{g.vendor}]")
    if s.cardwire:
        print(f"cardwire: mode {s.cw_mode}; available {list(s.cw_modes)}")
    else:
        print("cardwire: no")
    print(f"supergfxd: {'yes' if s.supergfx else 'no'}; mode {s.mode}; supported {list(s.supported)}")
    if s.asus_egpu:
        print(f"{xg_line(s)}; hardware mode {s.hw_mode}" + (f"; pending {s.hw_pending}" if s.hw_pending else ""))
        print(f"hardware switch backend: {'reboot (asus-gpu-switch@)' if s.reboot_backend else 'not installed'}")
    print(f"rendering: {describe(s)}")
    for m in s.cw_modes:
        print(f"  live mode {m}: {cw_label(m, s)}")
    for m in hw_modes(s):
        print(f"  hardware mode {m}: {hw_label(m, s)}")
    if not s.cardwire:
        for m in s.supported:
            print(f"  supergfxd mode {m}: {mode_label(m, s)}")


def hw_modes(s: GpuState) -> tuple[str, ...]:
    if not s.asus_egpu:
        return ()
    return ("Hybrid", "AsusEgpu") + (("AsusMuxDgpu",) if s.has_mux else ())


# --- GUI ---------------------------------------------------------------------

HERE = Path(__file__).resolve().parent
NVIDIA_SVG = next((p for p in (HERE / "nvidia.svg", HERE / "icons" / "nvidia.svg") if p.exists()), None)
BADGE = {"AMD": ("AMD", "#ed1c24"), "Intel": ("Intel", "#0071c5")}


def make_icon(g: Gpu | None, xg: bool) -> QIcon:
    size = 64
    pix = QPixmap(size, size)
    pix.fill(Qt.GlobalColor.transparent)
    p = QPainter(pix)
    p.setRenderHint(QPainter.RenderHint.Antialiasing)
    if g and g.vendor == "NVIDIA" and NVIDIA_SVG:
        p.drawPixmap(0, 0, QIcon(str(NVIDIA_SVG)).pixmap(size, size))
    else:
        label, bg = BADGE.get(g.vendor, ("GPU", "#7f8c8d")) if g else ("?", "#7f8c8d")
        p.setBrush(QColor(bg))
        p.setPen(Qt.PenStyle.NoPen)
        p.drawRoundedRect(QRectF(1, 1, size - 2, size - 2), 12, 12)
        font = QFont("Sans")
        font.setWeight(QFont.Weight.Black)
        font.setPixelSize(44)
        while QFontMetrics(font).horizontalAdvance(label) > size - 8 and font.pixelSize() > 10:
            font.setPixelSize(font.pixelSize() - 1)
        p.setFont(font)
        p.setPen(QColor("white"))
        p.drawText(QRectF(0, 0, size, size), Qt.AlignmentFlag.AlignCenter, label)
    if xg:
        # purple dot = an external GPU is attached (XG Mobile mode / Thunderbolt eGPU)
        p.setBrush(QColor("#9b59b6"))
        p.setPen(QColor("white"))
        p.drawEllipse(QRectF(size - 26, 0, 26, 26))
    p.end()
    return QIcon(pix)


class GpuTray(QSystemTrayIcon):
    def __init__(self) -> None:
        super().__init__()
        self.state: GpuState | None = None
        self.live_proc: subprocess.Popen | None = None
        self.live_timer = QTimer(self)
        self.live_timer.timeout.connect(self.check_live_switch)
        self.menu = QMenu()
        self.setContextMenu(self.menu)
        self.activated.connect(self.on_activated)
        self.timer = QTimer(self)
        self.timer.timeout.connect(self.refresh)
        self.timer.start(POLL_MS)
        self.refresh()
        self.show()

    def on_activated(self, reason: QSystemTrayIcon.ActivationReason) -> None:
        if reason == QSystemTrayIcon.ActivationReason.Trigger:
            self.refresh()
            self.showMessage("Active GPU", describe(self.state), self.icon(), 3000)

    def refresh(self) -> None:
        s = read_state()
        if s == self.state:
            return
        self.state = s
        self.setIcon(make_icon(working_gpu(s), s.egpu is not None))
        tip = [f"GPU: {describe(s)}"]
        if s.cardwire:
            tip.append(f"cardwire mode: {s.cw_mode.capitalize()}")
        elif s.supergfx:
            tip.append(f"supergfxd mode: {s.mode}")
        if s.asus_egpu:
            tip.append(xg_line(s))
        if s.hw_pending:
            tip.append(f"After reboot: {hw_label(s.hw_pending, s)}")
        elif s.pending:
            tip.append(f"Pending: {s.pending}" + (f" ({s.pending_action})" if s.pending_action else ""))
        self.setToolTip("\n".join(tip))
        self.build_menu(s)

    def radio_section(self, title: str, modes, current: str, label, handler, disabled=lambda m: "") -> None:
        m = self.menu
        m.addSection(title)
        group = QActionGroup(m)
        for mode in modes:
            a = QAction(label(mode), m, checkable=True)
            a.setChecked(mode == current)
            reason = disabled(mode)
            if reason:
                a.setText(f"{label(mode)} – {reason}")
                a.setEnabled(False)
            a.triggered.connect(lambda _=False, mode=mode: handler(mode))
            group.addAction(a)
            m.addAction(a)

    def build_menu(self, s: GpuState) -> None:
        m = self.menu
        m.clear()
        for g in s.gpus:
            m.addAction(gpu_line(g)).setEnabled(False)
        if not s.gpus:
            m.addAction("No graphics card detected").setEnabled(False)
        if s.asus_egpu:
            m.addAction(xg_line(s)).setEnabled(False)
        if s.hw_pending:
            m.addAction(f"After reboot: {hw_label(s.hw_pending, s)}").setEnabled(False)

        def xg_disabled(mode: str) -> str:
            return "connect and lock the dock" if mode == "AsusEgpu" and not s.egpu_connected else ""

        if s.cardwire:
            self.radio_section(
                "GPU access (live, cardwire)", s.cw_modes, s.cw_mode, lambda x: cw_label(x, s), self.switch_live
            )
            if s.asus_egpu:
                self.radio_section(
                    "Hardware (reboot)", hw_modes(s), s.hw_pending or s.hw_mode,
                    lambda x: hw_label(x, s), self.switch_hw, xg_disabled,
                )
            if s.asus_egpu and s.live_backend:
                self.add_live_menu(s)
        elif s.supergfx and s.supported:
            self.radio_section(
                f"Mode ({'switch with reboot' if s.reboot_backend else 'supergfxd'})", s.supported, s.mode,
                lambda x: mode_label(x, s), self.switch_supergfx,
                lambda x: xg_disabled(x) if s.asus_egpu else "",
            )
        else:
            m.addSeparator()
            m.addAction("Switching unavailable (install cardwire)").setEnabled(False)

        m.addSeparator()
        m.addAction("Refresh", self.force_refresh)
        m.addAction("Quit", QApplication.quit)

    def add_live_menu(self, s: GpuState) -> None:
        sub = self.menu.addMenu("Experimental: switch without reboot")
        if self.live_proc:
            sub.addAction("Switching in progress…").setEnabled(False)
            return
        for mode in ("Hybrid", "AsusEgpu"):
            a = sub.addAction(f"{hw_label(mode, s)} – live", lambda mode=mode: self.switch_hw_live(mode))
            a.setEnabled(mode != s.hw_mode and not s.hw_pending and (mode != "AsusEgpu" or s.egpu_connected))

    def switch_hw_live(self, mode: str) -> None:
        s = self.state or read_state()
        text = (
            f"EXPERIMENTAL: switch to {hw_label(mode, s)} without a reboot?\n\n"
            "The NVIDIA card is unplugged in software and the driver moves to the other card. "
            "It takes about 40 seconds. If anything still uses the card, the switch is aborted. "
            "If the firmware or the NVIDIA driver misbehaves, the system can freeze or reset.\n\n"
            "Save your work and close apps that use the NVIDIA GPU (games, ROG Control Center)."
        )
        if QMessageBox.warning(
            None, "Experimental GPU switch", text,
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No, QMessageBox.StandardButton.No,
        ) != QMessageBox.StandardButton.Yes:
            return
        self.live_proc = subprocess.Popen(
            ["systemctl", "start", f"asus-gpu-live@{mode}.service"],
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True,
        )
        self.live_timer.start(500)
        self.force_refresh()

    def check_live_switch(self) -> None:
        if not self.live_proc or self.live_proc.poll() is None:
            return
        self.live_timer.stop()
        rc, err = self.live_proc.returncode, self.live_proc.stderr.read()
        self.live_proc = None
        message = read(LIVE_RESULT) or err.strip() or f"systemctl exited with {rc}"
        if rc == 0:
            self.showMessage("GPU switched", message, self.icon(), 6000)
        else:
            QMessageBox.warning(None, "Experimental GPU switch", message)
        self.force_refresh()

    def force_refresh(self) -> None:
        self.state = None
        self.refresh()

    def switch_live(self, mode: str) -> None:
        s = self.state or read_state()
        if mode == s.cw_mode:
            return self.force_refresh()  # clicked the current mode - just restore the check mark
        res = subprocess.run(["cardwire", "set", mode], capture_output=True, text=True)
        if res.returncode != 0:
            QMessageBox.critical(None, "Change GPU mode", f"cardwire failed:\n{res.stderr or res.stdout}")
        else:
            note = "" if mode == "hybrid" else "\nApps already running keep their GPU until restarted."
            self.showMessage("GPU mode", f"{cw_label(mode, s)}{note}", self.icon(), 4000)
        self.force_refresh()

    def confirm_reboot(self, title: str, extra: str = "") -> bool:
        s = self.state
        text = (
            f"Switch to: {title}?\n\nThe computer will reboot right away and the change is applied "
            "during boot. Save your work."
        )
        if s and s.hw_mode == "AsusEgpu":
            text += "\n\nDisconnect the XG Mobile only once the new mode is active."
        answer = QMessageBox.question(None, "Change GPU mode", text + extra)
        return answer == QMessageBox.StandardButton.Yes

    def start_reboot_switch(self, mode: str) -> None:
        res = subprocess.run(
            ["systemctl", "start", f"asus-gpu-switch@{mode}.service"], capture_output=True, text=True
        )
        if res.returncode != 0:
            QMessageBox.critical(None, "Change GPU mode", f"Switching failed:\n{res.stderr or res.stdout}")

    def switch_hw(self, mode: str) -> None:
        s = self.state or read_state()
        if mode == (s.hw_pending or s.hw_mode):
            return self.force_refresh()
        if mode == "AsusEgpu" and read_attr("egpu_connected") != "1":
            QMessageBox.warning(None, "XG Mobile", "XG Mobile is not connected and locked.")
            return self.force_refresh()
        if not s.reboot_backend:
            QMessageBox.warning(
                None, "Change GPU mode", "The reboot-based switch backend is not installed. Run install.sh as root."
            )
            return self.force_refresh()
        if self.confirm_reboot(hw_label(mode, s)):
            self.start_reboot_switch(mode)
        self.force_refresh()

    def switch_supergfx(self, mode: str) -> None:
        s = self.state or read_state()
        if mode == s.mode:
            return self.force_refresh()
        if mode == "AsusEgpu" and s.asus_egpu and read_attr("egpu_connected") != "1":
            QMessageBox.warning(None, "XG Mobile", "XG Mobile is not connected and locked.")
            return self.force_refresh()
        if s.reboot_backend and mode in REBOOT_MODES:
            if self.confirm_reboot(mode_label(mode, s)):
                self.start_reboot_switch(mode)
            return self.force_refresh()
        how = "supergfxd will perform the change. It may require logging out or rebooting."
        if s.asus_egpu:
            how += (
                "\n\nWarning: live switching unloads the NVIDIA driver and has frozen ASUS laptops "
                "with nvidia-open. Run install.sh to get the safer reboot-based switching."
            )
        answer = QMessageBox.question(None, "Change GPU mode", f"Switch to: {mode_label(mode, s)}?\n\n{how}")
        if answer == QMessageBox.StandardButton.Yes:
            res = subprocess.run(["supergfxctl", "-m", mode], capture_output=True, text=True)
            if res.returncode != 0:
                QMessageBox.critical(None, "Change GPU mode", f"Switching failed:\n{res.stderr or res.stdout}")
            else:
                action = run("supergfxctl", "-p")
                if action and action not in ("Nothing", "None"):
                    QMessageBox.information(None, "Change GPU mode", f"supergfxd is waiting for: {action}")
        self.force_refresh()


def single_instance_lock():
    """Return the open lock file, or None when the tray already runs in this session."""
    runtime = os.environ.get("XDG_RUNTIME_DIR") or f"/tmp/asus-gpu-tray-{os.getuid()}"
    os.makedirs(runtime, exist_ok=True)
    lock = open(os.path.join(runtime, "asus-gpu-tray.lock"), "w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        lock.close()
        return None
    return lock


def main() -> None:
    if "--dump" in sys.argv:
        dump(read_state())
        return
    lock = single_instance_lock()
    if lock is None:
        run("notify-send", "-a", APP_NAME, "-i", "asus-gpu-tray", f"{APP_NAME} is already running",
            "The icon is in the system tray.")
        return
    app = QApplication(sys.argv)
    app.setQuitOnLastWindowClosed(False)
    app.setApplicationName("asus-gpu-tray")
    app.setDesktopFileName("asus-gpu-tray")
    fallback = HERE / "icons" / "asus-gpu-tray.svg"
    if not fallback.exists():
        fallback = HERE / "asus-gpu-tray.svg"
    app.setWindowIcon(QIcon.fromTheme("asus-gpu-tray", QIcon(str(fallback))))
    if not QSystemTrayIcon.isSystemTrayAvailable():
        print("No system tray available", file=sys.stderr)
        sys.exit(1)
    _tray = GpuTray()
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
