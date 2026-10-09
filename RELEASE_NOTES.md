## 2.0.0

Asus GPU Tray is now one Rust binary: the tray, and as subcommands the root helpers behind the
systemd units and the udev rule. No Python, PyQt6 or shell scripts at runtime. Version 1.x is
tagged `v1-python`.

- **Same behaviour.** The helpers (`switch-live`, `switch-reboot`, `switch-apply`, `egpu-power`) are
  line-by-line ports of the 1.x scripts. On a ROG Flow X16 GV601RE with the XG Mobile, the live
  switch both ways, the reboot switch both ways and the MUX round trip were recorded with the
  scripts and with the port: the steps are identical (`tests/reference/`).
- **Lighter.** About 18 MB of memory (RSS) on a GV601RE, with no Python or Qt to load.
- **MUX mode:** the tray no longer shows cardwire's modes there (they change nothing while the dGPU
  drives the panel); go back with Hardware → Built-in dGPU.
- **supergfxd fallback dropped.** Without cardwire the tray shows your GPUs and the ASUS hardware
  modes.

### Install

The archive holds the source tree and a binary built on Ubuntu 22.04 (glibc 2.35 or newer, x86-64):

```bash
sha256sum -c asus-gpu-tray-2.0.0-x86_64.tar.gz.sha256
tar xf asus-gpu-tray-2.0.0-x86_64.tar.gz
cd asus-gpu-tray-2.0.0
sudo ./install.sh
```

Installing over 1.x with `install.sh` removes the Python tray and the scripts. Arch users can build
the package from `packaging/aur` instead. The one-time KDE Plasma setup from the README is
unchanged.
