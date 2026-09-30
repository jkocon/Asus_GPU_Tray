#!/usr/bin/env python3
"""Asus GPU Tray: a system tray icon that shows which GPU is rendering and switches
supergfxd graphics modes. Detects graphics cards (iGPU / dGPU / eGPU) from sysfs,
reads the list of modes from supergfxd and shows only what the hardware supports.
XG Mobile and MUX options appear only on ASUS laptops with asus-armoury.
Without supergfxd it works as a read-only GPU viewer.

  asus_gpu_tray.py          system tray icon
  asus_gpu_tray.py --dump   print the detected state and exit"""

import fcntl
import os
import shutil
import subprocess
import sys
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
POLL_MS = 3000
VENDORS = {"10de": "NVIDIA", "1002": "AMD", "8086": "Intel"}
KIND_LABEL = {"igpu": "iGPU", "dgpu": "dGPU", "egpu": "eGPU"}


@dataclass(frozen=True)
class Gpu:
    addr: str
    vendor: str  # "NVIDIA" / "AMD" / "Intel" / raw vendor ID
    name: str
    driver: str
    kind: str  # igpu / dgpu / egpu
    power: str  # runtime_status from sysfs


@dataclass(frozen=True)
class GpuState:
    gpus: tuple[Gpu, ...]
    supergfx: bool
    mode: str
    supported: tuple[str, ...]
    dgpu_vendor: str
    pending: str
    pending_action: str
    asus_egpu: bool  # egpu_connected attribute exists (ASUS XG Mobile)
    egpu_connected: bool
    reboot_backend: bool

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


def detect_gpus() -> tuple[Gpu, ...]:
    # Only kernel-cached sysfs files - lspci reads config space and wakes a suspended dGPU.
    xg_active = read_attr("egpu_enable") == "1"
    gpus = []
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
            )
        )
    return tuple(gpus)


def read_state() -> GpuState:
    supergfx = shutil.which("supergfxctl") is not None
    mode = supported = dgpu_vendor = pending = action = ""
    if supergfx:
        mode = run("supergfxctl", "-g")
        supported = run("supergfxctl", "-s")
        dgpu_vendor = run("supergfxctl", "-V")
        pending = run("supergfxctl", "-P")
        action = run("supergfxctl", "-p")
    asus_egpu = (ATTR / "egpu_connected").exists()
    return GpuState(
        gpus=detect_gpus(),
        supergfx=supergfx and bool(mode),
        mode=mode or "?",
        supported=tuple(m.strip() for m in supported.strip("[]").split(",") if m.strip()),
        dgpu_vendor=dgpu_vendor,
        pending="" if pending in ("", "None", "Unknown") else pending,
        pending_action="" if action in ("", "Nothing", "None") else action,
        asus_egpu=asus_egpu,
        egpu_connected=read_attr("egpu_connected") == "1",
        reboot_backend=REBOOT_UNIT.exists() and asus_egpu,
    )


def dgpu_name(s: GpuState) -> str:
    if s.dgpu:
        return s.dgpu.name
    return f"built-in {s.dgpu_vendor}" if s.dgpu_vendor else "built-in dGPU"


def mode_label(mode: str, s: GpuState) -> str:
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


def working_gpu(s: GpuState) -> Gpu | None:
    """The card currently rendering (shown by the icon)."""
    ext = s.egpu or s.dgpu
    if s.mode in ("AsusEgpu", "AsusMuxDgpu") and ext:
        return s.egpu if s.mode == "AsusEgpu" and s.egpu else ext
    for g in (s.egpu, s.dgpu):
        if g and g.driver and g.power == "active":
            return g
    return s.igpu or (s.gpus[0] if s.gpus else None)


def power_label(g: Gpu) -> str:
    return g.power or "no runtime PM"


def describe(s: GpuState) -> str:
    g = working_gpu(s)
    if not g:
        return "No graphics card detected"
    others = [x for x in s.gpus if x is not g]
    text = f"{g.name} ({KIND_LABEL[g.kind]})"
    if others:
        text += " · " + ", ".join(f"{x.name} {power_label(x)}" for x in others)
    return text


def gpu_line(g: Gpu) -> str:
    return f"{KIND_LABEL[g.kind]}: {g.name} – {g.driver or 'no driver'}, {power_label(g)}"


def xg_line(s: GpuState) -> str:
    return f"XG Mobile: {'connected' if s.egpu_connected else 'not connected'}"


def dump(s: GpuState) -> None:
    for g in s.gpus:
        print(f"{g.addr}  {gpu_line(g)}  [{g.vendor}]")
    print(f"supergfxd: {'yes' if s.supergfx else 'no'}; mode {s.mode}; supported {list(s.supported)}")
    if s.asus_egpu:
        print(xg_line(s))
    print(f"switch backend: {'reboot (asus-gpu-switch@)' if s.reboot_backend else 'supergfxctl -m'}")
    print(f"rendering: {describe(s)}")
    for m in s.supported:
        print(f"  mode {m}: {mode_label(m, s)}")


# --- GUI ---------------------------------------------------------------------

HERE = Path(__file__).resolve().parent
NVIDIA_SVG = next((p for p in (HERE / "nvidia.svg", HERE / "icons" / "nvidia.svg") if p.exists()), None)
BADGE = {"AMD": ("AMD", "#ed1c24"), "Intel": ("Intel", "#0071c5")}


def make_icon(g: Gpu | None) -> QIcon:
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
    if g and g.kind == "egpu":
        # purple dot = external card
        p.setBrush(QColor("#9b59b6"))
        p.setPen(QColor("white"))
        p.drawEllipse(QRectF(size - 26, 0, 26, 26))
    p.end()
    return QIcon(pix)


class GpuTray(QSystemTrayIcon):
    def __init__(self) -> None:
        super().__init__()
        self.state: GpuState | None = None
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
        self.setIcon(make_icon(working_gpu(s)))
        tip = [f"GPU: {describe(s)}"]
        if s.supergfx:
            tip.append(f"supergfxd mode: {s.mode}")
        if s.asus_egpu:
            tip.append(xg_line(s))
        if s.pending:
            tip.append(f"Pending: {s.pending}" + (f" ({s.pending_action})" if s.pending_action else ""))
        self.setToolTip("\n".join(tip))
        self.build_menu(s)

    def build_menu(self, s: GpuState) -> None:
        m = self.menu
        m.clear()
        for g in s.gpus:
            m.addAction(gpu_line(g)).setEnabled(False)
        if not s.gpus:
            m.addAction("No graphics card detected").setEnabled(False)
        if s.asus_egpu:
            m.addAction(xg_line(s)).setEnabled(False)
        if s.pending:
            m.addAction(f"Pending change: {s.pending}").setEnabled(False)

        if s.supergfx and s.supported:
            m.addSection(f"Mode ({'switch with reboot' if s.reboot_backend else 'supergfxd'})")
            group = QActionGroup(m)
            for mode in s.supported:
                a = QAction(mode_label(mode, s), m, checkable=True)
                a.setChecked(mode == s.mode)
                if mode == "AsusEgpu" and s.asus_egpu and not s.egpu_connected:
                    a.setText(f"{mode_label(mode, s)} – connect and lock the dock")
                    a.setEnabled(False)
                a.triggered.connect(lambda _=False, mode=mode: self.switch(mode))
                group.addAction(a)
                m.addAction(a)
        elif not s.supergfx:
            m.addSeparator()
            m.addAction("Switching unavailable (supergfxd not running)").setEnabled(False)

        m.addSeparator()
        m.addAction("Refresh", self.force_refresh)
        m.addAction("Quit", QApplication.quit)

    def force_refresh(self) -> None:
        self.state = None
        self.refresh()

    def switch(self, mode: str) -> None:
        s = self.state or read_state()
        if mode == s.mode:
            self.force_refresh()  # clicked the current mode - just restore the check mark
            return
        if mode == "AsusEgpu" and s.asus_egpu and read_attr("egpu_connected") != "1":
            QMessageBox.warning(None, "XG Mobile", "XG Mobile is not connected and locked.")
            return self.force_refresh()
        reboot = s.reboot_backend and mode in REBOOT_MODES
        if reboot:
            how = "The computer will reboot right away and the mode will change during boot. Save your work."
        else:
            how = "supergfxd will perform the change. It may require logging out or rebooting."
            if s.asus_egpu:
                how += (
                    "\n\nWarning: live switching unloads the NVIDIA driver and has frozen ASUS laptops "
                    "with nvidia-open. Run install.sh to get the safer reboot-based switching."
                )
        if s.mode == "AsusEgpu":
            how += "\n\nDisconnect the XG Mobile only once the new mode is active."
        answer = QMessageBox.question(None, "Change GPU mode", f"Switch to: {mode_label(mode, s)}?\n\n{how}")
        if answer != QMessageBox.StandardButton.Yes:
            return self.force_refresh()
        cmd = ["systemctl", "start", f"asus-gpu-switch@{mode}.service"] if reboot else ["supergfxctl", "-m", mode]
        res = subprocess.run(cmd, capture_output=True, text=True)
        if res.returncode != 0:
            QMessageBox.critical(None, "Change GPU mode", f"Switching failed:\n{res.stderr or res.stdout}")
        elif not reboot:
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
