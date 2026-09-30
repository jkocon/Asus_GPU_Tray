"""Tests for asus_gpu_tray.py that need no ASUS hardware.

    QT_QPA_PLATFORM=offscreen python3 -m unittest discover -s tests
"""

import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import asus_gpu_tray as t  # noqa: E402
from PyQt6.QtCore import QCoreApplication, QEvent  # noqa: E402
from PyQt6.QtGui import QActionGroup  # noqa: E402
from PyQt6.QtWidgets import QApplication  # noqa: E402

APP = QApplication.instance() or QApplication([])

IGPU = t.Gpu("0000:3a:00.0", "AMD", "Radeon 680M", "amdgpu", "igpu", "active", False)
EGPU = t.Gpu("0000:01:00.0", "NVIDIA", "RTX 3070", "nvidia", "egpu", "suspended", False)
DGPU = t.Gpu("0000:01:00.0", "NVIDIA", "RTX 3050 Ti", "nvidia", "dgpu", "suspended", False)


def state(**kw) -> t.GpuState:
    base = dict(
        gpus=(EGPU, IGPU), cardwire=True, cw_mode="hybrid", cw_modes=("integrated", "hybrid", "smart"),
        supergfx=False, mode="?", supported=(), dgpu_vendor="", pending="", pending_action="",
        asus_egpu=True, egpu_connected=True, hw_mode="AsusEgpu", has_mux=True, hw_pending="",
        reboot_backend=True, live_backend=True,
    )
    base.update(kw)
    return t.GpuState(**base)


def flush_deletes() -> None:
    QCoreApplication.sendPostedEvents(None, QEvent.Type.DeferredDelete.value)


class Tray(t.GpuTray):
    """GpuTray with a fixed state instead of the real system."""

    current = state()

    def __init__(self) -> None:
        with mock.patch.object(t, "read_state", lambda: Tray.current):
            super().__init__()

    def refresh(self) -> None:
        with mock.patch.object(t, "read_state", lambda: Tray.current):
            super().refresh()


def texts(tray: t.GpuTray) -> list[str]:
    return [a.text() for a in tray.menu.actions() if a.text()]


class MenuTests(unittest.TestCase):
    def setUp(self) -> None:
        Tray.current = state()
        self.tray = Tray()

    def tearDown(self) -> None:
        self.tray.timer.stop()
        self.tray.hide()

    def test_no_action_group_leak(self) -> None:
        for _ in range(200):
            self.tray.build_menu(state())
            flush_deletes()
        self.assertLessEqual(len(self.tray.menu.findChildren(QActionGroup)), 2)

    def test_menu_is_not_rebuilt_while_open(self) -> None:
        before = texts(self.tray)
        self.tray.menu_open = True
        Tray.current = state(gpus=(DGPU, IGPU), hw_mode="Hybrid")
        self.tray.refresh()
        self.assertEqual(texts(self.tray), before)
        self.assertTrue(self.tray.menu_dirty)
        self.tray.on_menu_hide()
        self.assertNotEqual(texts(self.tray), before)
        self.assertFalse(self.tray.menu_dirty)

    def test_hardware_section_without_cardwire_or_supergfxd(self) -> None:
        self.tray.build_menu(state(cardwire=False, supergfx=False))
        items = texts(self.tray)
        self.assertIn("Hardware", items)
        self.assertNotIn("GPU access (live, cardwire)", items)
        self.assertIn("Built-in dGPU only (MUX) – reboot", items)

    def test_everything_disabled_while_switching(self) -> None:
        self.tray.live_proc = mock.Mock()
        self.tray.build_menu(state())
        switchable = [a for a in self.tray.menu.actions() if a.isCheckable()]
        self.assertTrue(switchable)
        self.assertTrue(all(not a.isEnabled() for a in switchable))
        self.tray.live_proc = None

    def test_ampersand_is_not_a_mnemonic(self) -> None:
        odd = t.Gpu("0000:02:00.0", "AMD", "R9 290 & 390", "amdgpu", "dgpu", "active", False)
        self.tray.build_menu(state(gpus=(odd, IGPU)))
        self.assertTrue(any("R9 290 && 390" in x for x in texts(self.tray)))


class LabelTests(unittest.TestCase):
    def test_parse_cardwire_get(self) -> None:
        mode, modes = t.parse_cardwire_get("Current Mode: Smart\nAvailable Mode: hybrid, manual, integrated")
        self.assertEqual(mode, "smart")
        self.assertEqual(modes, ("integrated", "hybrid", "manual"))

    def test_short_name_survives_odd_pci_ids_entries(self) -> None:
        with mock.patch.object(t, "pci_ids_name", lambda v, d: "Weird [name without end"):
            self.assertEqual(t.short_name("10de", "1234"), "Weird [name without end")
        with mock.patch.object(t, "pci_ids_name", lambda v, d: "GA104M [GeForce RTX 3070 Mobile / Max-Q]"):
            self.assertEqual(t.short_name("10de", "249d"), "RTX 3070")

    def test_can_switch_live(self) -> None:
        self.assertTrue(t.can_switch_live(state(), "Hybrid"))
        self.assertFalse(t.can_switch_live(state(), "AsusMuxDgpu"))
        self.assertFalse(t.can_switch_live(state(hw_mode="AsusMuxDgpu"), "Hybrid"))
        self.assertFalse(t.can_switch_live(state(hw_pending="Hybrid"), "Hybrid"))
        self.assertFalse(t.can_switch_live(state(live_backend=False), "Hybrid"))

    def test_working_gpu_ignores_sleeping_and_blocked_cards(self) -> None:
        self.assertIs(t.working_gpu(state()), IGPU)
        awake = t.Gpu(EGPU.addr, EGPU.vendor, EGPU.name, EGPU.driver, EGPU.kind, "active", False)
        self.assertIs(t.working_gpu(state(gpus=(awake, IGPU))), awake)
        blocked = t.Gpu(EGPU.addr, EGPU.vendor, EGPU.name, EGPU.driver, EGPU.kind, "active", True)
        self.assertIs(t.working_gpu(state(gpus=(blocked, IGPU))), IGPU)


class ProcessTests(unittest.TestCase):
    def spawn(self) -> subprocess.Popen:
        p = subprocess.Popen(["sleep", "30"])
        self.addCleanup(p.wait)
        self.addCleanup(p.kill)
        time.sleep(0.2)
        return p

    def test_stop_and_restart_command(self) -> None:
        p = self.spawn()
        found = [x for x in t.user_processes({"sleep"}) if x.pid == p.pid]
        self.assertEqual(len(found), 1)
        self.assertEqual(Path(found[0].exe).name, "sleep")
        self.assertEqual(found[0].restart_cmd()[1:], ["30"])
        t.stop_processes(found, timeout_s=2)
        self.assertIsNotNone(p.wait(timeout=2))

    def test_reused_pid_is_left_alone(self) -> None:
        p = self.spawn()
        stale = t.Proc(p.pid, "0", "/usr/bin/sleep", ("sleep", "30"))  # same PID, different process
        t.stop_processes([stale], timeout_s=0.3)
        self.assertIsNone(p.poll())

    def test_restart_uses_the_real_executable(self) -> None:
        proc = t.Proc(1, "1", "/usr/bin/rog-control-center", ("rog-control-center", "--autostart"))
        self.assertEqual(proc.restart_cmd(), ["/usr/bin/rog-control-center", "--autostart", "--background"])


class LockTests(unittest.TestCase):
    def test_lock_outside_tmp_and_single_instance(self) -> None:
        with tempfile.TemporaryDirectory() as home, mock.patch.dict(os.environ, {"HOME": home}):
            os.environ.pop("XDG_RUNTIME_DIR", None)
            fd = t.single_instance_lock()
            self.assertIsNotNone(fd)
            lock_dir = Path(home, ".cache", "asus-gpu-tray")
            self.assertEqual(lock_dir.stat().st_mode & 0o777, 0o700)
            self.assertIsNone(t.single_instance_lock())
            os.close(fd)

    def test_lock_does_not_follow_symlinks(self) -> None:
        with tempfile.NamedTemporaryFile("w", delete=False) as victim:
            victim.write("keep me")
        self.addCleanup(os.unlink, victim.name)
        with tempfile.TemporaryDirectory() as run, mock.patch.dict(os.environ, {"XDG_RUNTIME_DIR": run}):
            os.symlink(victim.name, os.path.join(run, "asus-gpu-tray.lock"))
            with self.assertRaises(OSError):
                t.single_instance_lock()
        self.assertEqual(Path(victim.name).read_text(), "keep me")


if __name__ == "__main__":
    unittest.main()
