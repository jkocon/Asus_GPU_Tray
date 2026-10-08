#!/bin/bash
# Experiment: bring the built-in dGPU back after a clean XG Mobile unlock (driver released, egpu_enable still 1).
attr=/sys/class/firmware-attributes/asus-armoury/attributes
root=0000:00:01.1
exec 9> /run/asus-gpu-tray.lock; flock -n 9 || { echo "switch lock busy"; exit 2; }
log() { echo "$(date +%T.%N | cut -c1-12) $*"; }
log "start: egpu_enable=$(<$attr/egpu_enable/current_value) egpu_connected=$(<$attr/egpu_connected/current_value)"
log "link ctl=$(setpci -s $root CAP_EXP+0x10.w) sta=$(setpci -s $root CAP_EXP+0x12.w)"
for f in 0000:01:00.1 0000:01:00.0; do
    d=/sys/bus/pci/devices/$f
    if [[ -e $d ]]; then
        log "driver of $f: $(basename "$(readlink $d/driver 2>/dev/null)" 2>/dev/null)"
        log "remove $f"; echo 1 > $d/remove; log "removed $f"
    fi
done
uemsk=$(setpci -s $root ECAP_AER+0x08.l 2>/dev/null)
[[ $uemsk =~ ^[0-9a-f]{8}$ ]] && setpci -s $root ECAP_AER+0x08.l=00000020:00000020
setpci -s $root CAP_EXP+0x10.w=0010:0010
log "link disabled"
sleep 0.5
log "egpu_enable -> 0"
echo 0 > $attr/egpu_enable/current_value || log "write failed"
echo 0 > $attr/dgpu_disable/current_value
log "egpu_enable now $(<$attr/egpu_enable/current_value)"
sleep 2
setpci -s $root CAP_EXP+0x10.w=0000:0010
log "link enabled"
for ((i = 0; i < 40; i++)); do
    (( 0x$(setpci -s $root CAP_EXP+0x12.w) & 0x2000 )) && { log "link up"; break; }
    sleep 0.5
done
if [[ $uemsk =~ ^[0-9a-f]{8}$ ]]; then setpci -s $root ECAP_AER+0x04.l=ffffffff; setpci -s $root ECAP_AER+0x08.l=$uemsk; fi
log "rescan"; echo 1 > /sys/bus/pci/rescan; log "rescan done"
for ((i = 0; i < 40; i++)); do
    drv=$(basename "$(readlink /sys/bus/pci/devices/0000:01:00.0/driver 2>/dev/null)" 2>/dev/null)
    [[ $drv == nvidia ]] && { log "01:00.0 bound to nvidia: $(cat /sys/bus/pci/devices/0000:01:00.0/device)"; exit 0; }
    sleep 0.5
done
log "no nvidia binding; devices: $(ls /sys/bus/pci/devices | grep 01:00)"
exit 1
