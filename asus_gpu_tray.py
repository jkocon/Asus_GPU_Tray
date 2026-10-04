#!/usr/bin/env python3
"""Asus GPU Tray: a system tray icon that shows which GPU is rendering and switches GPU modes.

- GPU access modes (Integrated / Hybrid / Smart) through cardwire - live, no reboot or logout.
- Hardware modes on ASUS laptops through asus-armoury: built-in dGPU <-> XG Mobile live
  (asus-gpu-live@.service), the MUX mode during the next boot (asus-gpu-switch@.service).
- supergfxd modes as a fallback when cardwire is not installed.
- Without any of them it works as a read-only GPU viewer.

The tray itself runs unprivileged; hardware changes go through the root-owned systemd units,
which a polkit rule lets local administrators start.

  asus_gpu_tray.py          system tray icon
  asus_gpu_tray.py --dump   print the detected state and exit"""

import fcntl
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
import traceback
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path

from PyQt6.QtCore import QProcess, QRectF, QSettings, Qt, QTimer
from PyQt6.QtGui import QAction, QActionGroup, QColor, QFont, QFontMetrics, QIcon, QPainter, QPixmap
from PyQt6.QtWidgets import (
    QApplication, QHBoxLayout, QLabel, QMenu, QMessageBox, QProgressBar, QPushButton, QSystemTrayIcon,
    QVBoxLayout, QWidget,
)

APP_NAME = "Asus GPU Tray"
ATTR = Path("/sys/class/firmware-attributes/asus-armoury/attributes")
PCI = Path("/sys/bus/pci/devices")
PCI_IDS = "/usr/share/hwdata/pci.ids"
# Reboot backend (switch at boot, before the NVIDIA driver loads); the polkit rule allows only these modes.
REBOOT_UNIT = Path("/etc/systemd/system/asus-gpu-switch@.service")
REBOOT_MODES = ("Integrated", "Hybrid", "AsusEgpu", "AsusMuxDgpu")
# Live hardware switch (built-in dGPU <-> XG Mobile) without a reboot.
LIVE_MODES = ("Hybrid", "AsusEgpu")
LIVE_UNIT = Path("/etc/systemd/system/asus-gpu-live@.service")
LIVE_RESULT = Path("/var/lib/asus-gpu-tray/live-result")
LIVE_PROGRESS = Path("/var/lib/asus-gpu-tray/live-progress")
LIVE_EXPECTED_S = 40
PENDING = Path("/var/lib/asus-gpu-tray/pending")
CMD_TIMEOUT_S = 20  # for commands started from the menu; polling commands use 5 s
# User apps that keep the NVIDIA card open, by executable name: closed before a live switch and
# started again after, with extra arguments so they come back the way they were (RCC: tray only).
RESTARTABLE_APPS = {"rog-control-center": ["--background"]}
# Never offered for killing when they hold the card: the desktop session would go down with them.
SESSION_PROCESSES = {"kwin_wayland", "kwin_x11", "Xwayland", "Xorg", "gnome-shell", "plasmashell", "systemd"}
# Kernel messages after which a GPU is gone until a reboot: Xid 79 and a failed wake-up from D3cold.
GPU_LOST_RE = "fallen off the bus|Unable to change power state from D3cold to D0"
POLL_MS = 3000
TRAY_WAIT_S = 300  # how long to wait for a system tray host at login
# After an unlock, how long to wait for the firmware: it removes the GPU within ~1 s and, after a
# clean release, switches back to the built-in dGPU within another second.
UNDOCK_WAIT_S = 5
UNDOCK_HINT = (
    "Before you unlock the XG Mobile, switch back to the built-in dGPU in the tray menu. "
    "Unlocking it while it is in use needs a reboot."
)
VENDORS = {"10de": "NVIDIA", "1002": "AMD", "8086": "Intel"}
KIND_LABEL = {"igpu": "iGPU", "dgpu": "dGPU", "egpu": "eGPU"}
CARDWIRE_MODES = ("integrated", "hybrid", "smart")  # order in the menu
REFRESH_EVERY_S = 60
CW_RESTARTS_MAX = 2  # cardwired restarts per episode when refresh-gpu does not fix the GPU list
_last_cw_refresh = 0.0
_cw_repairs = 0


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
    dgpu_disabled: bool = False  # asus-armoury dgpu_disable: the built-in dGPU is powered off on purpose

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
        return subprocess.run(cmd, capture_output=True, text=True, timeout=5, check=False).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return ""


def run_checked(cmd: list[str]) -> tuple[bool, str]:
    """Run a command started from the menu; (success, error output)."""
    try:
        res = subprocess.run(cmd, capture_output=True, text=True, timeout=CMD_TIMEOUT_S, check=False)
    except subprocess.TimeoutExpired:
        return False, f"{cmd[0]} did not answer within {CMD_TIMEOUT_S} s"
    except OSError as e:
        return False, str(e)
    return res.returncode == 0, (res.stderr or res.stdout).strip()


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
    if "[" in name and "]" in name:
        name = name[name.index("[") + 1 : name.rindex("]")] or name
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
        if not os.path.lexists(PCI / addr):
            continue  # removed from the bus (XG Mobile unplugged); cardwire still lists it
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
    global _last_cw_refresh, _cw_repairs
    cw = cardwire_devices()
    missed = bool(cw) and cardwire_missed_dgpu(cw)
    if missed and time.monotonic() - _last_cw_refresh > REFRESH_EVERY_S:
        _last_cw_refresh = time.monotonic()
        # refresh-gpu first; it did not always help (2026-10-04), restarting cardwired did. The
        # polkit rule allows exactly this restart.
        if _cw_repairs == 0:
            run("cardwire", "debug", "refresh-gpu")
        elif _cw_repairs <= CW_RESTARTS_MAX:
            run("systemctl", "--no-block", "restart", "cardwired.service")
        _cw_repairs += 1
        cw = cardwire_devices()
    elif not missed:
        _cw_repairs = 0
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
        hw_pending=read(PENDING),
        reboot_backend=REBOOT_UNIT.exists() and asus_egpu,
        live_backend=LIVE_UNIT.exists() and asus_egpu,
        dgpu_disabled=read_attr("dgpu_disable") == "1",
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


def parse_gpu_stats(out: str) -> str:
    """nvidia-smi "temperature, power, utilization, memory used, memory total" (csv, no units) ->
    "54 °C · 38 W · 12 % · 1.2/4.0 GB". Implausible or [N/A] fields are left out (the first power
    reading after a wake-up can be several hundred watts)."""
    fields = [f.strip() for f in out.splitlines()[0].split(",")] if out.strip() else []
    if len(fields) != 5:
        return ""

    def num(text: str, limit: float) -> float | None:
        try:
            value = float(text)
        except ValueError:
            return None
        return value if 0 <= value <= limit else None

    temp, power, util = num(fields[0], 150), num(fields[1], 400), num(fields[2], 100)
    used, total = num(fields[3], 1e6), num(fields[4], 1e6)
    parts = []
    if temp is not None:
        parts.append(f"{temp:.0f} °C")
    if power is not None:
        parts.append(f"{power:.0f} W")
    if util is not None:
        parts.append(f"{util:.0f} %")
    if used is not None and total:
        parts.append(f"{used / 1024:.1f}/{total / 1024:.1f} GB")
    return " · ".join(parts)


def gpu_stats(g: Gpu) -> str:
    """Live numbers of an NVIDIA GPU that is awake anyway. Never for a suspended or blocked card:
    nvidia-smi opens the device and would wake it. Only called when the user opens the menu or
    clicks the icon, never from the polling timer, so it does not keep the card awake either."""
    if g.vendor != "NVIDIA" or g.blocked or g.power != "active" or g.driver != "nvidia":
        return ""
    if not shutil.which("nvidia-smi"):
        return ""
    return parse_gpu_stats(run(
        "nvidia-smi", f"--id={g.addr}", "--format=csv,noheader,nounits",
        "--query-gpu=temperature.gpu,power.draw,utilization.gpu,memory.used,memory.total",
    ))


def on_battery() -> bool:
    supplies = list(Path("/sys/class/power_supply").glob("*"))
    mains = [p for p in supplies if read(p / "type") == "Mains"]
    return bool(mains) and not any(read(p / "online") == "1" for p in mains)


def gpu_users(g: Gpu) -> list[Proc]:
    """This user's processes that have this GPU itself open (/dev/nvidiaN or its DRM nodes) - not
    the control nodes (/dev/nvidiactl, -uvm, -modeset), which apps open just to list GPUs."""
    nodes = {str(n) for n in Path("/dev").glob("nvidia[0-9]*") if n.is_char_device()}
    nodes |= {f"/dev/dri/{n.name}" for n in (PCI / g.addr / "drm").glob("*") if n.name.startswith(("card", "renderD"))}
    return card_holders(nodes)


def working_gpu(s: GpuState) -> Gpu | None:
    """The card currently rendering (shown by the icon)."""
    for g in (s.egpu, s.dgpu):
        if g and g.driver and not g.blocked and g.power == "active":
            return g
    if s.hw_mode == "AsusMuxDgpu" or s.mode == "AsusMuxDgpu":
        return s.dgpu or s.igpu  # with the MUX the dGPU drives the panel
    return s.igpu or (s.gpus[0] if s.gpus else None)


@dataclass(frozen=True)
class Proc:
    pid: int
    start: str  # start time from /proc/<pid>/stat, so a reused PID is never mistaken for it
    exe: str
    args: tuple[str, ...]

    @property
    def name(self) -> str:
        return Path(self.exe).name

    def alive(self) -> bool:
        return proc_start(self.pid) == self.start

    def restart_cmd(self) -> list[str]:
        extra = RESTARTABLE_APPS.get(self.name, [])
        return [self.exe, *self.args[1:], *(a for a in extra if a not in self.args)]


def proc_start(pid: int) -> str:
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
    except OSError:
        return ""
    return stat.rsplit(")", 1)[-1].split()[19]  # field 22: starttime


def own_processes(all_users: bool = False):
    """This user's processes (everyone's with all_users, as root), except the tray itself
    (kernel threads have no executable)."""
    for proc in Path("/proc").iterdir():
        if not proc.name.isdigit() or int(proc.name) == os.getpid():
            continue
        try:
            if not all_users and proc.stat().st_uid != os.getuid():
                continue
            exe = os.readlink(proc / "exe").removesuffix(" (deleted)")
            args = tuple(a for a in (proc / "cmdline").read_bytes().decode(errors="replace").split("\0") if a)
        except OSError:
            continue
        yield Proc(int(proc.name), proc_start(int(proc.name)), exe, args)


def user_processes(names) -> list[Proc]:
    """This user's processes whose executable (not argv[0]) has one of the given names."""
    return [p for p in own_processes() if p.name in names]


def nvidia_nodes() -> set[str]:
    """/dev/nvidia* and the DRM nodes of the NVIDIA cards - what the live switch needs free."""
    nodes = {str(n) for n in Path("/dev").glob("nvidia*") if n.is_char_device()}
    nodes |= {str(n) for n in Path("/dev/nvidia-caps").glob("*")}
    for dev in PCI.iterdir():
        if read(dev / "vendor") == "0x10de":
            nodes |= {f"/dev/dri/{n.name}" for n in (dev / "drm").glob("*") if n.name.startswith(("card", "renderD"))}
    return nodes


def holds(pid: int, nodes: set[str]) -> bool:
    fd_dir = f"/proc/{pid}/fd"
    try:
        fds = os.listdir(fd_dir)
    except OSError:
        return False
    for fd in fds:
        try:
            if os.readlink(f"{fd_dir}/{fd}") in nodes:
                return True
        except OSError:
            pass  # closed in the meantime
    return False


def card_holders(nodes: set[str] | None = None, all_users: bool = False) -> list[Proc]:
    """This user's processes that have the NVIDIA card open. Reading /proc does not wake the card."""
    nodes = nvidia_nodes() if nodes is None else nodes
    return [p for p in own_processes(all_users) if holds(p.pid, nodes)] if nodes else []


def describe_procs(procs: list[Proc]) -> str:
    """One line per program: "firefox (PID 1234, 1240)"."""
    pids: dict[str, list[int]] = {}
    for p in procs:
        pids.setdefault(p.name, []).append(p.pid)
    lines = []
    for name, ids in sorted(pids.items()):
        more = f", … {len(ids)} processes" if len(ids) > 4 else ""
        lines.append(f"• {name} (PID {', '.join(map(str, ids[:4]))}{more})")
    return "\n".join(lines)


def stop_processes(procs: list[Proc], timeout_s: float = 5) -> None:
    def signal_alive(sig: int) -> None:
        for p in procs:
            if p.alive():
                try:
                    os.kill(p.pid, sig)
                except (ProcessLookupError, PermissionError):
                    pass

    signal_alive(signal.SIGTERM)
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline and any(p.alive() for p in procs):
        time.sleep(0.1)
    signal_alive(signal.SIGKILL)


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
    if xg_gone(s):
        return "XG Mobile: disconnected while in use – reboot needed"
    if xg_unlocked(s):
        return "XG Mobile: unlocked, still in use – do not disconnect"
    return f"XG Mobile: {'connected' if s.egpu_connected else 'not connected'}"


def dump(s: GpuState) -> None:
    for g in s.gpus:
        d3cold = read(PCI / g.addr / "d3cold_allowed")
        extra = f", D3cold {'allowed' if d3cold == '1' else 'disabled'}" if g.kind != "igpu" and d3cold else ""
        print(f"{g.addr}  {gpu_line(g)}{extra}  [{g.vendor}]")
    if s.cardwire:
        print(f"cardwire: mode {s.cw_mode}; available {list(s.cw_modes)}")
    else:
        print("cardwire: no")
    print(f"supergfxd: {'yes' if s.supergfx else 'no'}; mode {s.mode}; supported {list(s.supported)}")
    if s.asus_egpu:
        print(f"{xg_line(s)}; hardware mode {s.hw_mode}" + (f"; pending {s.hw_pending}" if s.hw_pending else ""))
        backends = [name for name, ok in (("live (asus-gpu-live@)", s.live_backend),
                                          ("reboot (asus-gpu-switch@)", s.reboot_backend)) if ok]
        print(f"hardware switch backends: {', '.join(backends) or 'not installed'}")
    print(f"rendering: {describe(s)}")
    for m in s.cw_modes:
        print(f"  live mode {m}: {cw_label(m, s)}")
    for m in hw_modes(s):
        print(f"  hardware mode {m}: {hw_label(m, s)}")
    if not s.cardwire:
        for m in s.supported:
            print(f"  supergfxd mode {m}: {mode_label(m, s)}")


def parse_gpu_lost(line: str) -> tuple[str, float] | None:
    """A `journalctl -o short-unix` line matching GPU_LOST_RE -> (PCI slot "0000:01:00", time)."""
    stamp, _, text = line.partition(" ")
    m = re.search(r"\b([0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2})\b", text)
    try:
        return (m.group(1), float(stamp)) if m else None
    except ValueError:
        return None


def last_live_switch() -> float:
    """When the last live switch succeeded: it re-creates the NVIDIA devices, so older errors are gone."""
    try:
        return LIVE_RESULT.stat().st_mtime if read(LIVE_RESULT).startswith("Switched") else 0.0
    except OSError:
        return 0.0


def lost_gpus(s: GpuState, events: dict[str, float], since: float) -> tuple[Gpu, ...]:
    """NVIDIA cards that fell off the bus: a kernel message newer than the card, or runtime PM in error."""
    return tuple(
        g for g in s.gpus
        if g.vendor == "NVIDIA" and (g.power == "error" or events.get(g.addr.rsplit(".", 1)[0], 0) > since)
    )


def xg_unlocked(s: GpuState) -> bool:
    """The XG Mobile is still the active GPU, but its lock was opened (or the cable pulled)."""
    return s.hw_mode == "AsusEgpu" and not s.egpu_connected and not s.hw_pending


def dgpu_missing(s: GpuState) -> bool:
    """Built-in dGPU mode, but the dGPU is not on the bus. What a clean undock leaves behind: with
    nothing holding the XG Mobile's GPU, the firmware switches back by itself (as on Windows) but
    the root port's link stays disabled. asus-gpu-live@Hybrid brings it back."""
    return s.asus_egpu and s.hw_mode == "Hybrid" and s.dgpu is None and not s.dgpu_disabled and not s.hw_pending


def xg_gone(s: GpuState) -> bool:
    """XG Mobile mode, but its GPU is no longer on the bus. Unlocking the XG Mobile makes the
    firmware drop the GPU at once (GV601RE: ~0.1 s after the lock event), and it does not come back
    when the dock is connected again; only a reboot recovers it."""
    return s.hw_mode == "AsusEgpu" and s.egpu is None and not s.hw_pending


# The NVIDIA driver wedges after losing a GPU, and a normal shutdown then hangs in it.
EMERGENCY_REBOOT_TEXT = (
    "A normal shutdown would hang in the NVIDIA driver, so the computer restarts the emergency way: "
    "the disks are synced and remounted read-only, then it resets at once. Open apps are not asked "
    "to quit - save your work first."
)

XG_GONE_TEXT = (
    "The XG Mobile was disconnected while it was the active GPU. Unlocking it disconnects the GPU "
    "at once, and it stays gone until a reboot, also after you connect it again. Apps that were "
    "using it may stop responding.\n\n"
    "Next time, switch to the built-in dGPU in this menu before unlocking the XG Mobile. (Unlocking "
    "works without a reboot only when nothing at all uses the GPU, system services included.)"
)


def can_switch_live(s: GpuState, mode: str) -> bool:
    """Built-in dGPU <-> XG Mobile can switch live; anything involving the MUX needs a reboot."""
    return s.live_backend and not s.hw_pending and mode in LIVE_MODES and s.hw_mode in LIVE_MODES


def hw_modes(s: GpuState) -> tuple[str, ...]:
    if not s.asus_egpu:
        return ()
    return ("Hybrid", "AsusEgpu") + (("AsusMuxDgpu",) if s.has_mux else ())


# --- GUI ---------------------------------------------------------------------

HERE = Path(__file__).resolve().parent
NVIDIA_SVG = next((p for p in (HERE / "nvidia.svg", HERE / "icons" / "nvidia.svg") if p.exists()), None)
BADGE = {"AMD": ("AMD", "#ed1c24"), "Intel": ("Intel", "#0071c5")}


def make_icon(g: Gpu | None, xg: bool, alert: bool = False) -> QIcon:
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
    if alert:
        # red dot = a GPU is lost or the XG Mobile is unlocked while in use
        p.setBrush(QColor("#e74c3c"))
        p.setPen(QColor("white"))
        p.drawEllipse(QRectF(size - 30, size - 30, 30, 30))
    p.end()
    return QIcon(pix)


def switch_stage(step: str, old: str, new: str) -> str:
    """A line of live-progress ("12:00:01 unbind ...") -> what the progress window says, or ""."""
    step = step.split(" ", 1)[-1]
    if step.startswith(("Live switch to", "Nobody holds")):
        return "Preparing…"
    if step.startswith(("unbind", "secondary bus reset", "remove")):
        return f"Disconnecting the {old}…"
    if step.startswith(("link down/up", "egpu_enable")):
        return "Switching the graphics lanes in the firmware…"
    if step.startswith(("enable link", "link on", "Rescanning")):
        return f"Connecting the {new}…"
    return ""


class SwitchWindow(QWidget):
    """Shown while a live switch runs, like the "Switching in progress" window of Armoury Crate.
    It cannot be closed until the switch is over. Plain raster widgets: showing it does not open
    any GPU device node, so it never gets in the way of the switch itself."""

    def __init__(self, target: str, old: str, new: str) -> None:
        flags = (
            Qt.WindowType.Dialog | Qt.WindowType.CustomizeWindowHint | Qt.WindowType.WindowTitleHint
            | Qt.WindowType.WindowStaysOnTopHint
        )
        super().__init__(None, flags)
        self.setWindowTitle(APP_NAME)
        self.old, self.new = old, new
        self.started = time.monotonic()
        self.done = False

        icon = QLabel()
        icon.setPixmap(QApplication.windowIcon().pixmap(48, 48))
        icon.setAlignment(Qt.AlignmentFlag.AlignTop)
        self.heading = QLabel(f"Switching to {target}…")
        font = self.heading.font()
        font.setPointSizeF(font.pointSizeF() * 1.3)
        font.setBold(True)
        self.heading.setFont(font)
        self.heading.setWordWrap(True)
        self.info = QLabel(
            "Switching in progress, please wait. Do not disconnect the XG Mobile, close the lid or "
            "shut down the computer."
        )
        self.info.setWordWrap(True)
        self.stage = QLabel("Preparing…")
        self.bar = QProgressBar()
        self.bar.setRange(0, 0)  # busy: the firmware call gives no progress
        self.bar.setTextVisible(False)
        self.elapsed = QLabel()
        self.elapsed.setEnabled(False)  # greyed out
        self.button = QPushButton("Close")
        self.button.clicked.connect(self.close)
        self.button.hide()

        text = QVBoxLayout()
        for w in (self.heading, self.info, self.stage, self.bar, self.elapsed):
            text.addWidget(w)
        buttons = QHBoxLayout()
        buttons.addStretch()
        buttons.addWidget(self.button)
        text.addLayout(buttons)
        row = QHBoxLayout(self)
        row.setContentsMargins(20, 20, 20, 16)
        row.setSpacing(16)
        row.addWidget(icon)
        row.addLayout(text)
        self.setFixedWidth(460)
        self.update_progress("")

    def update_progress(self, step: str) -> None:
        stage = switch_stage(step, self.old, self.new)
        if stage:
            self.stage.setText(stage)
        t = int(time.monotonic() - self.started)
        self.elapsed.setText(f"{t // 60}:{t % 60:02d} · usually about {LIVE_EXPECTED_S} seconds")

    def finish(self, ok: bool) -> None:
        """Failure: close right away, a dialog follows. Success: say so, close after 10 s."""
        self.done = True
        if not ok:
            self.close()
            return
        self.heading.setText(f"Switched to the {self.new}")
        self.info.setText(
            "You can disconnect the XG Mobile now." if self.new != "XG Mobile" else UNDOCK_HINT
        )
        self.stage.hide()
        t = int(time.monotonic() - self.started)
        self.elapsed.setText(f"Took {t // 60}:{t % 60:02d}")
        self.bar.setRange(0, 1)
        self.bar.setValue(1)
        self.button.show()
        QTimer.singleShot(10000, self.close)

    def closeEvent(self, event) -> None:
        if self.done:
            event.accept()
        else:
            event.ignore()  # Alt+F4 while the firmware is switching changes nothing - keep it visible


def menu_text(text: str) -> str:
    return text.replace("&", "&&")  # a single & would turn the next letter into a mnemonic


def message(icon: QMessageBox.Icon, title: str, text: str, question: bool = False) -> bool:
    """Plain-text message box - the text can contain process and device names. True for Yes."""
    box = QMessageBox(icon, title, text)
    box.setTextFormat(Qt.TextFormat.PlainText)
    if question:
        box.setStandardButtons(QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No)
    return box.exec() == QMessageBox.StandardButton.Yes


def ask(title: str, text: str) -> bool:
    return message(QMessageBox.Icon.Question, title, text, question=True)


class GpuTray(QSystemTrayIcon):
    def __init__(self) -> None:
        super().__init__()
        self.state: GpuState | None = None
        self.live_proc: subprocess.Popen | None = None
        self.live_mode = ""
        self.live_started = 0.0
        self.restart_after_live: list[list[str]] = []
        self.progress_win: SwitchWindow | None = None
        self.live_retries = 0
        self.xg_problem: bool | None = None  # XG Mobile unlocked or gone while active; None before the first poll
        self.undock_pending = False
        self.undock_wait = 0  # seconds waited for the firmware after an unlock
        self.xg_was_locked: bool | None = None  # None before the first poll: no question at startup
        self.settings = QSettings("asus-gpu-tray", "asus-gpu-tray")
        self.gpu_stats: dict[str, str] = {}  # PCI address -> live numbers, filled when the menu opens
        self.wake_polls = 0  # polls the built-in dGPU has been awake in a row
        self.wake_told: dict[tuple[str, ...], float] = {}  # app names -> when they were reported
        self.dock_pending = False
        self.lost: tuple[Gpu, ...] = ()
        self.lost_events: dict[str, float] = {}  # PCI slot -> time of the last "GPU lost" kernel message
        self.lost_asked: set[str] = set()
        self.kernel_log_buf = ""
        self.kernel_log = QProcess(self)
        self.kernel_log.readyReadStandardOutput.connect(self.on_kernel_log)
        self.live_timer = QTimer(self)
        self.live_timer.timeout.connect(self.check_live_switch)
        self.menu = QMenu()
        self.menu_open = False  # never rebuild under the cursor: items would shift while clicking
        self.menu_dirty = False
        self.menu.aboutToShow.connect(self.on_menu_show)
        self.menu.aboutToHide.connect(self.on_menu_hide)
        self.setContextMenu(self.menu)
        self.activated.connect(self.on_activated)
        self.timer = QTimer(self)
        self.timer.timeout.connect(self.refresh)
        self.timer.start(POLL_MS)
        self.start_kernel_log()
        self.refresh()
        self.show()

    def start_kernel_log(self) -> None:
        """Follow the kernel log for lost GPUs, from the start of this boot. Needs read access to the
        system journal (groups wheel, adm or systemd-journal); without it the tray relies on sysfs."""
        self.kernel_log.start(
            "journalctl", ["-k", "-b", "-f", "-n", "all", "-o", "short-unix", "--no-pager", "--grep", GPU_LOST_RE]
        )

    def on_kernel_log(self) -> None:
        self.kernel_log_buf += bytes(self.kernel_log.readAllStandardOutput()).decode(errors="replace")
        *lines, self.kernel_log_buf = self.kernel_log_buf.split("\n")
        for line in lines:
            hit = parse_gpu_lost(line)
            if hit:
                self.lost_events[hit[0]] = max(hit[1], self.lost_events.get(hit[0], 0.0))
        if lines:
            self.refresh()

    def on_activated(self, reason: QSystemTrayIcon.ActivationReason) -> None:
        if reason == QSystemTrayIcon.ActivationReason.Trigger:
            self.refresh()
            text = describe(self.state)
            g = working_gpu(self.state)
            stats = gpu_stats(g) if g else ""
            if stats:
                text += f"\n{stats}"
            self.showMessage("Active GPU", text, self.icon(), 4000)

    def on_menu_show(self) -> None:
        self.menu_open = False
        s = read_state()
        self.gpu_stats = {g.addr: gpu_stats(g) for g in s.gpus}
        self.force_refresh()  # open the menu with the current state
        self.menu_open = True

    def notify_wake_enabled(self) -> bool:
        return self.settings.value("notify_dgpu_wake", True, type=bool)

    def set_notify_wake(self, on: bool) -> None:
        self.settings.setValue("notify_dgpu_wake", on)

    def check_wake(self, s: GpuState) -> None:
        """On battery: say which apps woke the built-in dGPU once it has been awake for two polls
        (~6 s). Each set of apps at most every 10 minutes."""
        g = s.dgpu
        awake = bool(g and g.power == "active" and not g.blocked and g.driver)
        self.wake_polls = self.wake_polls + 1 if awake else 0
        if self.wake_polls != 2 or not self.notify_wake_enabled() or not on_battery():
            return
        names = tuple(sorted({p.name for p in gpu_users(g)}))
        if not names or time.monotonic() - self.wake_told.get(names, -1e9) < 600:
            return
        self.wake_told[names] = time.monotonic()
        self.showMessage(
            f"{g.name} woke up", f"On battery, used by: {', '.join(names)}", self.icon(), 8000,
        )

    def on_menu_hide(self) -> None:
        self.menu_open = False
        if self.menu_dirty and self.state:
            self.build_menu(self.state)

    def refresh(self) -> None:
        s = read_state()
        self.check_wake(s)
        # During a live switch the cards are removed and re-created on purpose.
        lost = () if self.live_proc else lost_gpus(s, self.lost_events, last_live_switch())
        self.watch(s, lost)
        if s == self.state and lost == self.lost:
            return
        self.state = s
        self.lost = lost
        alert = bool(lost) or (not self.live_proc and (xg_unlocked(s) or xg_gone(s) or dgpu_missing(s)))
        self.setIcon(make_icon(working_gpu(s), s.egpu is not None, alert))
        tip = [f"GPU: {describe(s)}"]
        tip += [f"{g.name} fell off the bus – reboot needed" for g in lost]
        if s.cardwire:
            tip.append(f"cardwire mode: {s.cw_mode.capitalize()}")
        elif s.supergfx:
            tip.append(f"supergfxd mode: {s.mode}")
        if s.asus_egpu:
            tip.append(xg_line(s))
        if self.live_proc:
            tip.append(f"Switching to {hw_label(self.live_mode, s)}…")
        if s.hw_pending:
            tip.append(f"After reboot: {hw_label(s.hw_pending, s)}")
        elif s.pending:
            tip.append(f"Pending: {s.pending}" + (f" ({s.pending_action})" if s.pending_action else ""))
        self.setToolTip("\n".join(tip))
        if self.menu_open:
            self.menu_dirty = True
        else:
            self.build_menu(s)

    def watch(self, s: GpuState, lost: tuple[Gpu, ...]) -> None:
        """React to the XG Mobile being locked or unlocked and to lost GPUs - once no dialog is open."""
        problem = xg_unlocked(s) or xg_gone(s) or dgpu_missing(s)
        if self.xg_problem is False and problem and not self.live_proc:
            self.undock_pending = True
        if not problem:
            self.undock_pending = False
        self.xg_problem = problem
        # Dock connected and locked while on the built-in dGPU: offer to switch to it.
        if self.xg_was_locked is False and s.egpu_connected and not self.live_proc:
            self.dock_pending = True
        if not s.egpu_connected or s.hw_mode != "Hybrid":
            self.dock_pending = False
        self.xg_was_locked = s.egpu_connected
        if self.live_proc or QApplication.activeModalWidget() is not None:
            return
        new_lost = tuple(g for g in lost if g.addr not in self.lost_asked)
        if new_lost:
            self.lost_asked |= {g.addr for g in new_lost}
            QTimer.singleShot(0, lambda: self.ask_reboot_lost(new_lost))
        elif self.undock_pending:
            self.undock_pending = False
            QTimer.singleShot(0, self.on_xg_unlocked)
        elif self.dock_pending:
            self.dock_pending = False
            QTimer.singleShot(0, self.on_xg_locked)

    def radio_section(self, title: str, modes, current: str, label, handler, disabled=lambda m: "") -> None:
        m = self.menu
        m.addSection(title)
        group = QActionGroup(m)
        for mode in modes:
            a = QAction(menu_text(label(mode)), m, checkable=True)
            a.setChecked(mode == current)
            reason = disabled(mode)
            if reason:
                a.setText(menu_text(f"{label(mode)} – {reason}"))
                a.setEnabled(False)
            a.triggered.connect(lambda _=False, mode=mode: handler(mode))
            group.addAction(a)
            m.addAction(a)

    def build_menu(self, s: GpuState) -> None:
        m = self.menu
        self.menu_dirty = False
        m.clear()  # deletes the actions, but not the QActionGroups parented to the menu
        for group in m.findChildren(QActionGroup):
            group.deleteLater()
        for g in s.gpus:
            m.addAction(menu_text(gpu_line(g))).setEnabled(False)
            if self.gpu_stats.get(g.addr):
                m.addAction(menu_text(f"    {self.gpu_stats[g.addr]}")).setEnabled(False)
        if not s.gpus:
            m.addAction("No graphics card detected").setEnabled(False)
        for g in self.lost:
            m.addAction(menu_text(f"⚠ {g.name} fell off the bus – Reboot…"), lambda _=False, g=g: self.ask_reboot_lost((g,)))
        if s.asus_egpu:
            m.addAction(xg_line(s)).setEnabled(False)
        if xg_gone(s) and not self.live_proc:
            m.addAction("⚠ XG Mobile disconnected – Reboot…", lambda _=False: self.offer_reboot(s, "Hybrid", XG_GONE_TEXT))
        if dgpu_missing(s) and s.live_backend and not self.live_proc:
            m.addAction("⚠ Built-in dGPU missing – Bring it back", lambda _=False: self.start_live("Hybrid"))
        if s.hw_pending:
            m.addAction(menu_text(f"After reboot: {hw_label(s.hw_pending, s)}")).setEnabled(False)

        def busy(_mode: str) -> str:
            return "switching…" if self.live_proc else ""

        def xg_disabled(mode: str) -> str:
            if self.live_proc:
                return "switching…"
            if mode == "AsusEgpu" and not s.egpu_connected:
                return "connect and lock the dock"
            if mode == "AsusMuxDgpu" and s.hw_mode == "AsusEgpu":
                return "switch to the built-in dGPU first"  # the firmware refuses it (EBUSY)
            return ""

        def hw_item(mode: str) -> str:
            current = mode == (s.hw_pending or s.hw_mode)
            refused = mode == "AsusMuxDgpu" and s.hw_mode == "AsusEgpu"  # xg_disabled explains why
            if mode == "Hybrid" and s.hw_mode == "AsusEgpu":
                return hw_label(mode, s) + " – before undocking"
            return hw_label(mode, s) + ("" if current or refused or can_switch_live(s, mode) else " – reboot")

        if s.cardwire:
            self.radio_section(
                "GPU access (live, cardwire)", s.cw_modes, s.cw_mode, lambda x: cw_label(x, s), self.switch_live, busy
            )
        elif s.supergfx and s.supported:
            # Modes the Hardware section already covers are left out on ASUS laptops.
            modes = [x for x in s.supported if x not in hw_modes(s)]
            if modes:
                self.radio_section(
                    f"Mode ({'switch with reboot' if s.reboot_backend else 'supergfxd'})", modes, s.mode,
                    lambda x: mode_label(x, s), self.switch_supergfx, busy,
                )
        # The hardware switch needs only asus-armoury - neither cardwire nor supergfxd.
        if s.asus_egpu:
            self.radio_section(
                "Hardware", hw_modes(s), s.hw_pending or s.hw_mode, hw_item, self.switch_hw, xg_disabled,
            )
        if not s.cardwire and not s.supergfx and not s.asus_egpu:
            m.addSeparator()
            m.addAction("Switching unavailable (install cardwire)").setEnabled(False)

        if s.hw_mode == "AsusEgpu" and can_switch_live(s, "Hybrid") and not self.live_proc:
            m.addSeparator()
            m.addAction("Undock now (close all GPU apps)…", self.undock_now)

        m.addSeparator()
        if s.dgpu:
            wake = QAction("Notify when the dGPU wakes up on battery", m, checkable=True)
            wake.setChecked(self.notify_wake_enabled())
            wake.toggled.connect(self.set_notify_wake)
            m.addAction(wake)
        m.addAction("Refresh", self.force_refresh)
        m.addAction("Quit", QApplication.quit)

    def force_refresh(self) -> None:
        self.state = None
        self.refresh()

    # --- cardwire ------------------------------------------------------------------------------

    def switch_live(self, mode: str) -> None:
        s = self.state or read_state()
        if mode == s.cw_mode or self.live_proc:
            return self.force_refresh()  # clicked the current mode - just restore the check mark
        ok, err = run_checked(["cardwire", "set", mode])
        if not ok:
            message(QMessageBox.Icon.Critical, "Change GPU mode", f"cardwire failed:\n{err}")
        else:
            note = "" if mode == "hybrid" else "\nApps already running keep their GPU until restarted."
            self.showMessage("GPU mode", f"{cw_label(mode, s)}{note}", self.icon(), 4000)
        self.force_refresh()

    # --- hardware (ASUS) ------------------------------------------------------------------------

    def switch_hw(self, mode: str) -> None:
        s = self.state or read_state()
        if mode == (s.hw_pending or s.hw_mode) or self.live_proc:
            return self.force_refresh()
        if mode == "AsusEgpu" and read_attr("egpu_connected") != "1":
            message(QMessageBox.Icon.Warning, "XG Mobile", "XG Mobile is not connected and locked.")
            return self.force_refresh()
        if can_switch_live(s, mode):
            return self.switch_hw_live(mode)
        if not s.reboot_backend:
            message(QMessageBox.Icon.Warning, "Change GPU mode",
                    "The reboot-based switch backend is not installed. Run install.sh as root.")
            return self.force_refresh()
        if self.confirm_reboot(hw_label(mode, s)):
            self.start_reboot_switch(mode)
        self.force_refresh()

    def switch_hw_live(self, mode: str, intro: str = "") -> None:
        """Live switch built-in dGPU <-> XG Mobile. intro goes before the question."""
        s = self.state or read_state()
        self.live_retries = 0
        if self.lost or xg_gone(s) or xg_unlocked(s):
            # A lost GPU, or one the firmware is removing: the NVIDIA driver is (about to be) wedged,
            # and the live switch would hang in it. Only a reboot gets out of this.
            gone = ", ".join(g.name for g in self.lost) or "The XG Mobile GPU"
            self.offer_reboot(s, mode, XG_GONE_TEXT if not self.lost else f"{gone} is lost, so the switch needs a reboot.")
            return self.force_refresh()
        text = (
            f"{intro}Switch to: {hw_label(mode, s)}?\n\n"
            "No reboot needed; it takes about 40 seconds. Apps that use the NVIDIA GPU will be "
            "listed first, so you can close them."
        )
        if user_processes(RESTARTABLE_APPS):
            text += "\n\nROG Control Center will be closed and started again afterwards."
        if not ask("XG Mobile connected" if intro else "Change GPU mode", text):
            return self.force_refresh()
        if self.free_card(s, mode):
            self.start_live(mode)
        self.force_refresh()

    def undock_now(self) -> None:
        """One confirmation, then close every app that has the XG Mobile's GPU open and switch to
        the built-in dGPU live, so the dock can be unplugged. For when there is no time to go
        through the apps one by one. It must happen before the XG Mobile is unlocked: afterwards
        the firmware has already dropped the GPU, and closing the apps would hang in the driver."""
        s = self.state or read_state()
        if self.live_proc:
            return
        self.live_retries = 0
        if self.lost or xg_gone(s) or xg_unlocked(s):
            self.offer_reboot(s, "Hybrid", XG_GONE_TEXT)
            return self.force_refresh()
        procs = [p for p in card_holders() if p.name not in RESTARTABLE_APPS]
        session = [p for p in procs if p.name in SESSION_PROCESSES]
        if session:
            message(
                QMessageBox.Icon.Warning, "Undock now",
                f"The desktop session itself uses the XG Mobile:\n\n{describe_procs(session)}\n\n"
                "These cannot be closed without ending the session. See the KDE Plasma setup in the README.",
            )
            self.offer_reboot(s, "Hybrid", "")
            return self.force_refresh()
        text = "Switch to the built-in dGPU now, so the XG Mobile can be unplugged?\n\n"
        if procs:
            text += (
                f"These apps use the XG Mobile and will be closed without asking again; unsaved work "
                f"in them is lost:\n\n{describe_procs(procs)}\n\n"
            )
        text += (
            "ROG Control Center is closed and started again, and the GPU services are stopped during "
            "the switch. It takes about 35 seconds. Keep the XG Mobile locked until it is done."
        )
        if not ask("Undock now", text):
            return self.force_refresh()
        # Again: apps may have opened the GPU while the dialog was up.
        procs = [p for p in card_holders() if p.name not in RESTARTABLE_APPS and p.name not in SESSION_PROCESSES]
        stop_processes(procs)
        left = [p for p in procs if p.alive()]
        if left:
            message(QMessageBox.Icon.Warning, "Undock now", f"These apps did not quit:\n\n{describe_procs(left)}")
            self.offer_reboot(s, "Hybrid", "")
            return self.force_refresh()
        self.start_live("Hybrid")
        self.force_refresh()

    def free_card(self, s: GpuState, mode: str) -> bool:
        """True when none of this user's apps (except restartable ones) holds the NVIDIA card any
        more. Otherwise lists them and offers to kill them; if the user says no, offers a reboot."""
        procs = [p for p in card_holders() if p.name not in RESTARTABLE_APPS]
        if not procs:
            return True
        gpu = s.egpu or s.dgpu
        name = f"{gpu.name} ({KIND_LABEL[gpu.kind]})" if gpu else "NVIDIA GPU"
        session = [p for p in procs if p.name in SESSION_PROCESSES]
        if session:
            message(
                QMessageBox.Icon.Warning, "GPU in use",
                f"The desktop session itself uses the {name}:\n\n{describe_procs(session)}\n\n"
                "These cannot be closed without ending the session. See the KDE Plasma setup in the README.",
            )
        elif ask(
            "GPU in use",
            f"These apps are using the {name}:\n\n{describe_procs(procs)}\n\n"
            "Kill them to continue the switch? Unsaved work in them will be lost.",
        ):
            stop_processes(procs)
            left = [p for p in procs if p.alive()]
            if not left:
                return True
            message(QMessageBox.Icon.Warning, "GPU in use", f"These apps did not quit:\n\n{describe_procs(left)}")
        self.offer_reboot(s, mode, "")
        return False

    def offer_reboot(self, s: GpuState, mode: str, why: str) -> None:
        """Ask to reboot and switch during boot instead of live."""
        if not s.reboot_backend:
            message(QMessageBox.Icon.Warning, "Change GPU mode",
                    (why + "\n\n" if why else "") + "The reboot-based switch backend is not installed.")
            return
        text = (
            (why + "\n\n" if why else "")
            + f"Reboot now and switch to {hw_label(mode, s)} during boot?\n\n"
            "Save your work first: the computer restarts right away."
        )
        if self.lost or xg_gone(s) or xg_unlocked(s):
            text += "\n\n" + EMERGENCY_REBOOT_TEXT
        elif s.hw_mode == "AsusEgpu":
            text += "\n\nDisconnect the XG Mobile only once the computer has restarted."
        if ask("Reboot", text):
            self.start_reboot_switch(mode)

    def on_xg_locked(self) -> None:
        """The XG Mobile was connected and locked while the built-in dGPU is active: offer the
        live switch to it (the same dialogs as from the menu)."""
        s = read_state()
        if self.live_proc or not s.egpu_connected or s.hw_mode != "Hybrid" or self.lost or dgpu_missing(s):
            return
        if not can_switch_live(s, "AsusEgpu"):
            return  # no live backend, or a reboot switch is pending: the menu still offers what works
        self.state = s
        self.switch_hw_live("AsusEgpu", intro="The XG Mobile is connected and locked.\n\n")

    def on_xg_unlocked(self) -> None:
        """The XG Mobile was unlocked while it was the active GPU. The firmware removes its GPU at
        once (0.1-1.1 s on a GV601RE), so there is never time for a live switch: one started in
        that window hung in the kernel. What follows tells the two outcomes apart:
        - nothing held the GPU, the driver let it go, and the firmware switched back to the built-in
          dGPU by itself: only the link and a rescan are missing - done live, no reboot;
        - something held it: egpu_enable stays 1 and the NVIDIA driver is wedged - reboot."""
        s = read_state()
        if self.live_proc:
            return
        if dgpu_missing(s) and s.live_backend:
            self.undock_wait = 0
            self.showMessage("XG Mobile released", "Bringing back the built-in dGPU…", self.icon(), 5000)
            self.start_live("Hybrid")
            return
        if not (xg_gone(s) or xg_unlocked(s)):
            self.undock_wait = 0
            return
        if self.undock_wait < UNDOCK_WAIT_S:
            self.undock_wait += 1
            QTimer.singleShot(1000, self.on_xg_unlocked)
            return
        self.undock_wait = 0
        self.showMessage("XG Mobile disconnected", "It was unlocked while in use - a reboot is needed.",
                         QSystemTrayIcon.MessageIcon.Critical, 10000)
        why = XG_GONE_TEXT
        # The apps still have the dead GPU open; services such as cardwired and nvidia-powerd do
        # too, but run as root and cannot be seen from here.
        procs = card_holders()
        if procs:
            why += f"\n\nStill holding the GPU:\n{describe_procs(procs)}"
        why += "\n\nWithout a reboot the built-in dGPU stays unavailable until the next boot."
        self.offer_reboot(s, "Hybrid", why)
        self.force_refresh()

    def start_live(self, mode: str) -> None:
        s = self.state or read_state()
        to_xg = mode == "AsusEgpu"
        self.progress_win = SwitchWindow(
            hw_label(mode, s), "built-in dGPU" if to_xg else "XG Mobile", "XG Mobile" if to_xg else "built-in dGPU"
        )
        self.progress_win.show()
        self.progress_win.raise_()
        self.progress_win.activateWindow()
        QApplication.processEvents()  # paint it before closing ROG Control Center blocks for a moment
        apps = user_processes(RESTARTABLE_APPS)  # now: the dialogs may have been open for a while
        stop_processes(apps)
        self.restart_after_live = [p.restart_cmd() for p in apps]
        self.live_mode = mode
        self.live_started = time.time()
        self.live_proc = subprocess.Popen(
            ["systemctl", "start", f"asus-gpu-live@{mode}.service"],
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True,
        )
        self.live_timer.start(500)
        self.force_refresh()

    def live_step(self) -> str:
        """The last line of live-progress, if it belongs to the switch that is running."""
        try:
            if LIVE_PROGRESS.stat().st_mtime < self.live_started - 1:
                return ""
        except OSError:
            return ""
        lines = read(LIVE_PROGRESS).splitlines()
        return lines[-1] if lines else ""

    def check_live_switch(self) -> None:
        if not self.live_proc:
            return
        if self.live_proc.poll() is None:
            if self.progress_win:
                self.progress_win.update_progress(self.live_step())
            return
        self.live_timer.stop()
        rc, err = self.live_proc.returncode, self.live_proc.stderr.read()
        self.live_proc = None
        for cmd in self.restart_after_live:
            try:
                subprocess.Popen(cmd, start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            except OSError:
                pass
        self.restart_after_live = []
        # The result file is left over from the previous switch if the unit never ran (polkit said no).
        try:
            fresh = LIVE_RESULT.stat().st_mtime >= self.live_started - 1
        except OSError:
            fresh = False
        message_text = (read(LIVE_RESULT) if fresh else "") or err.strip() or f"systemctl exited with {rc}"
        if self.progress_win:
            self.progress_win.finish(rc == 0)
        s = self.state or read_state()
        if rc == 0:
            if self.live_mode == "Hybrid":
                message_text += "\nYou can disconnect the XG Mobile now."
            else:
                message_text += "\n" + UNDOCK_HINT
            self.showMessage("GPU switched", message_text, self.icon(), 10000)
        elif message_text.startswith("Aborted, the NVIDIA card is still in use") and self.live_retries < 2:
            # Something opened the card after the check. Ask about this user's apps again.
            self.live_retries += 1
            if not card_holders():
                self.offer_reboot(s, self.live_mode, message_text)
            elif self.free_card(s, self.live_mode):
                self.start_live(self.live_mode)
        else:
            self.offer_reboot(s, self.live_mode, message_text)
        self.force_refresh()

    def ask_reboot_lost(self, gpus: tuple[Gpu, ...]) -> None:
        names = ", ".join(g.name for g in gpus)
        s = self.state or read_state()
        # Through the reboot unit when there is one: it reboots the emergency way after a GPU loss.
        if xg_unlocked(s) or xg_gone(s):
            mode = "Hybrid"
        else:
            mode = s.hw_pending or s.hw_mode
        via_unit = s.reboot_backend and mode in REBOOT_MODES
        text = (
            f"{names} fell off the PCIe bus. Apps can no longer use it, and it comes back only after a "
            "reboot.\n\nReboot now?"
        )
        if via_unit:
            text += "\n\n" + EMERGENCY_REBOOT_TEXT
            if mode == "Hybrid" and s.hw_mode == "AsusEgpu":
                text += "\n\nThe XG Mobile was unlocked, so the computer will start on the built-in dGPU."
        else:
            text += " Save your work first."
        if not ask("GPU lost", text):
            return
        if via_unit:
            return self.start_reboot_switch(mode)
        ok, err = run_checked(["systemctl", "reboot"])
        if not ok:
            message(QMessageBox.Icon.Critical, "Reboot", f"Reboot failed:\n{err}")

    def confirm_reboot(self, title: str) -> bool:
        s = self.state
        text = (
            f"Switch to: {title}?\n\nThe computer will reboot right away and the change is applied "
            "during boot. Save your work."
        )
        if s and s.hw_mode == "AsusEgpu":
            text += "\n\nDisconnect the XG Mobile only once the new mode is active."
        return ask("Change GPU mode", text)

    def start_reboot_switch(self, mode: str) -> None:
        unit = f"asus-gpu-switch@{mode}.service"
        ok, err = run_checked(["systemctl", "start", unit])
        if not ok:
            message(QMessageBox.Icon.Critical, "Change GPU mode", f"Switching failed:\n{err}")
            return
        # The unit is Type=simple, so "start" succeeds even when the script refuses; the reboot
        # follows within a second when it does not.
        QTimer.singleShot(3000, lambda: self.check_reboot_unit(unit))

    def check_reboot_unit(self, unit: str) -> None:
        if run("systemctl", "is-failed", unit) != "failed":
            return
        why = run("journalctl", "-b", "-u", unit, "-n", "1", "-o", "cat", "--no-pager")
        message(QMessageBox.Icon.Warning, "Change GPU mode", f"The switch was not scheduled:\n{why or unit + ' failed'}")

    # --- supergfxd fallback ---------------------------------------------------------------------

    def switch_supergfx(self, mode: str) -> None:
        s = self.state or read_state()
        if mode == s.mode or self.live_proc:
            return self.force_refresh()
        if mode == "AsusEgpu" and s.asus_egpu and read_attr("egpu_connected") != "1":
            message(QMessageBox.Icon.Warning, "XG Mobile", "XG Mobile is not connected and locked.")
            return self.force_refresh()
        if s.asus_egpu and can_switch_live(s, mode):
            return self.switch_hw_live(mode)
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
        if ask("Change GPU mode", f"Switch to: {mode_label(mode, s)}?\n\n{how}"):
            ok, err = run_checked(["supergfxctl", "-m", mode])
            if not ok:
                message(QMessageBox.Icon.Critical, "Change GPU mode", f"Switching failed:\n{err}")
            else:
                action = run("supergfxctl", "-p")
                if action and action not in ("Nothing", "None"):
                    message(QMessageBox.Icon.Information, "Change GPU mode", f"supergfxd is waiting for: {action}")
        self.force_refresh()


def single_instance_lock() -> int | None:
    """Return the locked file descriptor, or None when the tray already runs in this session."""
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if not runtime or not os.path.isdir(runtime):
        # Private per-user directory; never a predictable path in /tmp that someone else could plant.
        runtime = os.path.join(os.path.expanduser("~"), ".cache", "asus-gpu-tray")
        os.makedirs(runtime, mode=0o700, exist_ok=True)
    flags = os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC
    fd = os.open(os.path.join(runtime, "asus-gpu-tray.lock"), flags, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        os.close(fd)
        return None
    return fd


def log_exception(exc_type, exc, tb) -> None:
    # PyQt aborts the whole app on an exception in a slot unless sys.excepthook is replaced.
    traceback.print_exception(exc_type, exc, tb)


def main() -> None:
    if "--dump" in sys.argv:
        dump(read_state())
        return
    if "--holders" in sys.argv:
        # As root this sees every process; the undock experiment needs nobody holding the card.
        procs = card_holders(all_users=os.geteuid() == 0)
        print(describe_procs(procs) if procs else "Nobody holds the NVIDIA card")
        sys.exit(1 if procs else 0)
    try:
        lock = single_instance_lock()
    except OSError as e:
        sys.exit(f"Cannot create the single-instance lock: {e}")
    if lock is None:
        run("notify-send", "-a", APP_NAME, "-i", "asus-gpu-tray", f"{APP_NAME} is already running",
            "The icon is in the system tray.")
        return
    sys.excepthook = log_exception
    app = QApplication(sys.argv)
    app.setQuitOnLastWindowClosed(False)
    app.setApplicationName("asus-gpu-tray")
    app.setDesktopFileName("asus-gpu-tray")
    fallback = HERE / "icons" / "asus-gpu-tray.svg"
    if not fallback.exists():
        fallback = HERE / "asus-gpu-tray.svg"
    app.setWindowIcon(QIcon.fromTheme("asus-gpu-tray", QIcon(str(fallback))))
    trays: list[GpuTray] = []  # keeps the icon alive

    def start(deadline: float = time.monotonic() + TRAY_WAIT_S) -> None:
        # At login the panel may come up late (it did by ~25 s when a driver held up the boot);
        # without a tray host, wait for one instead of exiting.
        if QSystemTrayIcon.isSystemTrayAvailable():
            trays.append(GpuTray())
        elif time.monotonic() < deadline:
            QTimer.singleShot(2000, lambda: start(deadline))
        else:
            print(f"No system tray available after {TRAY_WAIT_S} s", file=sys.stderr)
            app.exit(1)

    start()
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
