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

    def start_kernel_log(self) -> None:
        pass


def process_events() -> None:
    for _ in range(3):
        QCoreApplication.processEvents()


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
        self.assertIn("Built-in dGPU only (MUX) – switch to the built-in dGPU first", items)

    def test_everything_disabled_while_switching(self) -> None:
        self.tray.live_proc = mock.Mock()
        self.tray.build_menu(state())
        switchable = [a for a in self.tray.menu.actions() if a.isCheckable()]
        self.assertTrue(switchable)
        self.assertTrue(all(not a.isEnabled() for a in switchable))
        self.tray.live_proc = None

    def test_mux_disabled_in_xg_mobile_mode(self) -> None:
        self.tray.build_menu(state())
        self.assertIn("Built-in dGPU only (MUX) – switch to the built-in dGPU first", texts(self.tray))
        self.tray.build_menu(state(gpus=(DGPU, IGPU), hw_mode="Hybrid"))
        self.assertIn("RTX 3050 Ti only (MUX) – reboot", texts(self.tray))

    def test_refused_reboot_switch_is_reported(self) -> None:
        with mock.patch.object(t, "run_checked", return_value=(True, "")), \
                mock.patch.object(t, "run", side_effect=["failed", "The firmware refused the MUX mode"]), \
                mock.patch.object(t, "message") as msg:
            self.tray.start_reboot_switch("AsusMuxDgpu")
            self.tray.check_reboot_unit("asus-gpu-switch@AsusMuxDgpu.service")
        self.assertIn("The firmware refused the MUX mode", msg.call_args.args[2])

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


class UndockTests(unittest.TestCase):
    """Unlocking the XG Mobile while it is the active GPU starts the switch to the built-in dGPU."""

    def setUp(self) -> None:
        Tray.current = state()
        self.tray = Tray()
        self.addCleanup(self.tray.timer.stop)
        self.addCleanup(self.tray.hide)
        self.switch = mock.patch.object(self.tray, "switch_hw_live").start()
        mock.patch.object(t, "read_state", lambda: Tray.current).start()  # on_xg_unlocked reads it again
        self.addCleanup(mock.patch.stopall)

    def unlock(self, attr_value: str) -> None:
        Tray.current = state(egpu_connected=False)
        with mock.patch.object(t, "read_attr", lambda name: attr_value), \
                mock.patch.object(t, "read_state", lambda: Tray.current):
            self.tray.refresh()
            process_events()

    def test_unlock_never_starts_a_live_switch(self) -> None:
        # GPU still on the bus right after the lock event: the firmware is about to remove it, and
        # a live switch started in that window hung in the kernel (2026-10-04).
        offer = mock.patch.object(self.tray, "offer_reboot").start()
        self.unlock("0")
        self.switch.assert_not_called()
        offer.assert_called_once()
        self.assertEqual(offer.call_args.args[1], "Hybrid")
        self.assertIn("XG Mobile: unlocked, still in use – do not disconnect", texts(self.tray))

    def test_manual_switch_while_unlocked_offers_reboot(self) -> None:
        offer = mock.patch.object(self.tray, "offer_reboot").start()
        live = mock.patch.object(self.tray, "start_live").start()
        self.tray.state = state(egpu_connected=False)
        t.GpuTray.switch_hw_live(self.tray, "Hybrid")  # the real one; setUp mocks it on the instance
        live.assert_not_called()
        offer.assert_called_once()

    def test_gpu_already_gone_offers_reboot(self) -> None:
        offer = mock.patch.object(self.tray, "offer_reboot").start()
        Tray.current = state(gpus=(IGPU,), egpu_connected=False)
        with mock.patch.object(t, "read_attr", lambda name: "0"):
            self.tray.refresh()
            process_events()
        self.switch.assert_not_called()
        offer.assert_called_once()
        self.assertIs(offer.call_args.args[2], t.XG_GONE_TEXT)
        self.assertIn("XG Mobile: disconnected while in use – reboot needed", texts(self.tray))
        self.assertIn("⚠ XG Mobile disconnected – Reboot…", texts(self.tray))

    def test_gone_after_reconnecting_the_dock(self) -> None:
        offer = mock.patch.object(self.tray, "offer_reboot").start()
        Tray.current = state(gpus=(IGPU,), egpu_connected=True)  # locked again, GPU still missing
        self.tray.refresh()
        process_events()
        offer.assert_called_once()

    def test_no_dialog_when_the_tray_starts_in_that_state(self) -> None:
        offer = mock.patch.object(t.GpuTray, "offer_reboot").start()
        Tray.current = state(gpus=(IGPU,), egpu_connected=False)
        tray = Tray()
        self.addCleanup(tray.timer.stop)
        self.addCleanup(tray.hide)
        process_events()
        offer.assert_not_called()

    def test_undocking_in_builtin_mode_is_normal(self) -> None:
        Tray.current = state(gpus=(DGPU, IGPU), hw_mode="Hybrid")
        self.tray.refresh()
        Tray.current = state(gpus=(DGPU, IGPU), hw_mode="Hybrid", egpu_connected=False)
        with mock.patch.object(t, "read_attr", lambda name: "0"):
            self.tray.refresh()
            process_events()
        self.switch.assert_not_called()


class UndockHintTests(unittest.TestCase):
    def setUp(self) -> None:
        Tray.current = state()
        self.tray = Tray()
        self.addCleanup(self.tray.timer.stop)
        self.addCleanup(self.tray.hide)
        self.addCleanup(mock.patch.stopall)
        # A refresh would queue another on_xg_unlocked with a real dialog for a later test.
        mock.patch.object(self.tray, "force_refresh").start()

    def test_menu_says_switch_back_before_undocking(self) -> None:
        self.tray.build_menu(state())
        self.assertIn("Built-in dGPU – before undocking", texts(self.tray))
        self.tray.build_menu(state(gpus=(DGPU, IGPU), hw_mode="Hybrid"))
        self.assertNotIn("before undocking", " ".join(texts(self.tray)))

    def test_window_after_switching_to_xg_reminds(self) -> None:
        w = t.SwitchWindow("XG Mobile", "built-in dGPU", "XG Mobile")
        w.finish(True)
        self.assertEqual(w.info.text(), t.UNDOCK_HINT)
        w.close()

    def test_experimental_clean_undock_tries_live_switch(self) -> None:
        mock.patch.object(t, "EXPERIMENTAL", True).start()
        mock.patch.object(t, "read_state", lambda: Tray.current).start()
        mock.patch.object(t, "ask", return_value=True).start()
        live = mock.patch.object(self.tray, "start_live").start()
        offer = mock.patch.object(self.tray, "offer_reboot").start()
        Tray.current = state(gpus=(IGPU,), egpu_connected=False)
        self.tray.on_xg_unlocked()
        live.assert_called_once_with("Hybrid")
        offer.assert_not_called()

    def test_experimental_declined_offers_reboot(self) -> None:
        mock.patch.object(t, "EXPERIMENTAL", True).start()
        mock.patch.object(t, "read_state", lambda: Tray.current).start()
        mock.patch.object(t, "ask", return_value=False).start()
        live = mock.patch.object(self.tray, "start_live").start()
        offer = mock.patch.object(self.tray, "offer_reboot").start()
        Tray.current = state(gpus=(IGPU,), egpu_connected=False)
        self.tray.on_xg_unlocked()
        live.assert_not_called()
        offer.assert_called_once()

    def test_without_experimental_only_reboot(self) -> None:
        mock.patch.object(t, "read_state", lambda: Tray.current).start()
        live = mock.patch.object(self.tray, "start_live").start()
        offer = mock.patch.object(self.tray, "offer_reboot").start()
        Tray.current = state(gpus=(IGPU,), egpu_connected=False)
        self.tray.on_xg_unlocked()
        live.assert_not_called()
        offer.assert_called_once()


class DetectTests(unittest.TestCase):
    def test_removed_gpu_is_not_taken_from_cardwire(self) -> None:
        cw = {"0000:01:00.0": {"blocked": True, "discrete": True, "vendor": "Nvidia", "name": "NVIDIA GeForce RTX 3070"}}
        with tempfile.TemporaryDirectory() as d, mock.patch.object(t, "PCI", Path(d)), \
                mock.patch.object(t, "read_attr", return_value="1"):
            self.assertEqual(t.detect_gpus(cw), ())
            Path(d, "0000:01:00.0").mkdir()  # still on the bus, only hidden by cardwire
            self.assertEqual([g.name for g in t.detect_gpus(cw)], ["RTX 3070"])


class SwitchDialogTests(unittest.TestCase):
    """Apps holding the card: kill them, or else reboot."""

    def setUp(self) -> None:
        Tray.current = state()
        self.tray = Tray()
        self.addCleanup(self.tray.timer.stop)
        self.addCleanup(self.tray.hide)
        self.addCleanup(mock.patch.stopall)
        self.game = t.Proc(4242, "1", "/usr/bin/game", ("game",))
        self.holders = mock.patch.object(t, "card_holders", return_value=[self.game]).start()
        self.stop = mock.patch.object(t, "stop_processes").start()
        self.reboot = mock.patch.object(self.tray, "start_reboot_switch").start()
        self.message = mock.patch.object(t, "message").start()

    def answers(self, *replies: bool) -> mock.Mock:
        return mock.patch.object(t, "ask", side_effect=list(replies)).start()

    def test_kill_yes(self) -> None:
        ask = self.answers(True)
        with mock.patch.object(t.Proc, "alive", return_value=False):
            self.assertTrue(self.tray.free_card(state(), "Hybrid"))
        self.assertIn("game (PID 4242)", ask.call_args.args[1])
        self.assertIn("RTX 3070 (eGPU)", ask.call_args.args[1])
        self.stop.assert_called_once_with([self.game])
        self.reboot.assert_not_called()

    def test_kill_no_then_reboot_yes(self) -> None:
        ask = self.answers(False, True)
        self.assertFalse(self.tray.free_card(state(), "Hybrid"))
        self.stop.assert_not_called()
        self.assertIn("Reboot now", ask.call_args.args[1])
        self.reboot.assert_called_once_with("Hybrid")

    def test_kill_no_then_reboot_no(self) -> None:
        self.answers(False, False)
        self.assertFalse(self.tray.free_card(state(), "Hybrid"))
        self.reboot.assert_not_called()
        self.stop.assert_not_called()

    def test_session_processes_are_never_killed(self) -> None:
        self.holders.return_value = [t.Proc(1, "1", "/usr/bin/kwin_wayland", ("kwin_wayland",))]
        ask = self.answers(True)
        self.assertFalse(self.tray.free_card(state(), "Hybrid"))
        self.stop.assert_not_called()
        self.assertIn("Reboot now", ask.call_args.args[1])
        self.reboot.assert_called_once()

    def test_restartable_apps_are_not_listed(self) -> None:
        self.holders.return_value = [t.Proc(1, "1", "/usr/bin/rog-control-center", ("rog-control-center",))]
        self.assertTrue(self.tray.free_card(state(), "Hybrid"))

    def test_lost_gpu_skips_the_live_switch(self) -> None:
        self.tray.lost = (EGPU,)
        ask = self.answers(True)
        live = mock.patch.object(self.tray, "start_live").start()
        self.tray.switch_hw_live("Hybrid")
        live.assert_not_called()
        self.assertIn("RTX 3070 is lost", ask.call_args.args[1])
        self.reboot.assert_called_once_with("Hybrid")


class LostGpuTests(unittest.TestCase):
    XID = "1759600000.5 host kernel: NVRM: Xid (PCI:0000:01:00): 79, pid='<unknown>', GPU has fallen off the bus."
    D3 = "1759600001.0 host kernel: nvidia 0000:01:00.0: Unable to change power state from D3cold to D0, device inaccessible"

    def test_parse(self) -> None:
        self.assertEqual(t.parse_gpu_lost(self.XID), ("0000:01:00", 1759600000.5))
        self.assertEqual(t.parse_gpu_lost(self.D3), ("0000:01:00", 1759600001.0))
        self.assertIsNone(t.parse_gpu_lost("-- No entries --"))

    def test_only_errors_newer_than_the_last_switch(self) -> None:
        events = {"0000:01:00": 100.0}
        self.assertEqual(t.lost_gpus(state(), events, since=50.0), (EGPU,))
        self.assertEqual(t.lost_gpus(state(), events, since=150.0), ())
        broken = t.Gpu(EGPU.addr, EGPU.vendor, EGPU.name, EGPU.driver, EGPU.kind, "error", False)
        self.assertEqual(t.lost_gpus(state(gpus=(broken, IGPU)), {}, since=0.0), (broken,))

    def test_tray_asks_once_and_shows_menu_item(self) -> None:
        Tray.current = state()
        tray = Tray()
        self.addCleanup(tray.timer.stop)
        self.addCleanup(tray.hide)
        with mock.patch.object(tray, "ask_reboot_lost") as asked, mock.patch.object(t, "last_live_switch", return_value=0.0):
            tray.kernel_log_buf = self.XID + "\n"
            tray.lost_events["0000:01:00"] = 1759600000.5
            tray.refresh()
            tray.refresh()
            process_events()
        asked.assert_called_once_with((EGPU,))
        self.assertIn("⚠ RTX 3070 fell off the bus – Reboot…", texts(tray))


class LostRebootTests(unittest.TestCase):
    """After a GPU loss the reboot goes through the reboot unit, which reboots the emergency way."""

    def setUp(self) -> None:
        Tray.current = state()
        self.tray = Tray()
        self.addCleanup(self.tray.timer.stop)
        self.addCleanup(self.tray.hide)
        self.addCleanup(mock.patch.stopall)
        self.ask = mock.patch.object(t, "ask", return_value=True).start()
        self.reboot = mock.patch.object(self.tray, "start_reboot_switch").start()
        self.plain = mock.patch.object(t, "run_checked", return_value=(True, "")).start()

    def test_lost_xg_mobile_keeps_its_mode(self) -> None:
        self.tray.state = state()
        self.tray.ask_reboot_lost((EGPU,))
        self.reboot.assert_called_once_with("AsusEgpu")
        self.assertIn("emergency way", self.ask.call_args.args[1])

    def test_gone_xg_mobile_starts_on_builtin(self) -> None:
        self.tray.state = state(gpus=(IGPU,), egpu_connected=False)
        self.tray.ask_reboot_lost((EGPU,))
        self.reboot.assert_called_once_with("Hybrid")
        self.assertIn("start on the built-in dGPU", self.ask.call_args.args[1])

    def test_without_backend_plain_reboot(self) -> None:
        self.tray.state = state(reboot_backend=False, asus_egpu=False, hw_mode="")
        self.tray.ask_reboot_lost((DGPU,))
        self.reboot.assert_not_called()
        self.plain.assert_called_once_with(["systemctl", "reboot"])

    def test_offer_reboot_warns_about_emergency_reboot(self) -> None:
        self.tray.offer_reboot(state(gpus=(IGPU,)), "Hybrid", t.XG_GONE_TEXT)
        self.assertIn("emergency way", self.ask.call_args.args[1])
        self.reboot.assert_called_once_with("Hybrid")


class HolderTests(unittest.TestCase):
    def test_card_holders_finds_open_node(self) -> None:
        with tempfile.NamedTemporaryFile() as node, open(node.name) as held:
            p = subprocess.Popen(["sleep", "30"], stdin=held)
            self.addCleanup(p.wait)
            self.addCleanup(p.kill)
            time.sleep(0.2)
            found = [x.pid for x in t.card_holders({node.name})]
            self.assertIn(p.pid, found)
            self.assertNotIn(os.getpid(), found)
            self.assertEqual(t.card_holders(set()), [])

    def test_describe_groups_by_program(self) -> None:
        procs = [t.Proc(i, "1", "/usr/lib/firefox/firefox", ()) for i in range(1, 7)]
        self.assertEqual(t.describe_procs(procs), "• firefox (PID 1, 2, 3, 4, … 6 processes)")


class SwitchWindowTests(unittest.TestCase):
    def test_stages_follow_the_live_script(self) -> None:
        stage = lambda step: t.switch_stage(step, "XG Mobile", "built-in dGPU")  # noqa: E731
        self.assertEqual(stage("21:00:00 Live switch to Hybrid: stopping services"), "Preparing…")
        self.assertEqual(stage("21:00:02 unbind 0000:01:00.0 from nvidia"), "Disconnecting the XG Mobile…")
        self.assertEqual(stage("21:00:03 egpu_enable -> 0"), "Switching the graphics lanes in the firmware…")
        self.assertEqual(stage("21:00:31 Rescanning PCI"), "Connecting the built-in dGPU…")
        self.assertEqual(stage("21:00:35 Switched to Hybrid without a reboot (0000:01:00.0)"), "")

    def test_cannot_be_closed_while_switching(self) -> None:
        w = t.SwitchWindow("XG Mobile", "built-in dGPU", "XG Mobile")
        w.show()
        self.assertFalse(w.close())
        self.assertTrue(w.isVisible())
        w.update_progress("21:00:03 egpu_enable -> 1")
        self.assertEqual(w.stage.text(), "Switching the graphics lanes in the firmware…")
        w.update_progress("21:00:04 something unknown")
        self.assertEqual(w.stage.text(), "Switching the graphics lanes in the firmware…")
        w.finish(True)
        self.assertIn("Switched to the XG Mobile", w.heading.text())
        self.assertTrue(w.close())

    def test_tray_shows_and_finishes_the_window(self) -> None:
        Tray.current = state()
        tray = Tray()
        self.addCleanup(tray.timer.stop)
        self.addCleanup(tray.hide)
        with tempfile.TemporaryDirectory() as d, \
                mock.patch.object(t, "LIVE_PROGRESS", Path(d, "live-progress")), \
                mock.patch.object(t, "LIVE_RESULT", Path(d, "live-result")), \
                mock.patch.object(t.subprocess, "Popen") as popen, \
                mock.patch.object(t, "user_processes", return_value=[]), \
                mock.patch.object(t, "read_attr", return_value="0"):
            popen.return_value.poll.return_value = None
            tray.start_live("Hybrid")
            tray.live_timer.stop()
            win = tray.progress_win
            self.assertTrue(win.isVisible())
            self.assertIn("Built-in dGPU", win.heading.text())
            Path(d, "live-progress").write_text("21:00:00 Live switch to Hybrid: stopping services\n21:00:02 remove x\n")
            tray.check_live_switch()
            self.assertEqual(win.stage.text(), "Disconnecting the XG Mobile…")
            Path(d, "live-result").write_text("Switched to Hybrid without a reboot (0000:01:00.0)")
            popen.return_value.poll.return_value = 0
            popen.return_value.returncode = 0
            popen.return_value.stderr.read.return_value = ""
            tray.check_live_switch()
            self.assertEqual(win.info.text(), "You can disconnect the XG Mobile now.")
            win.close()


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
