#!/bin/sh
set -eu
set -o pipefail
nonce=@NONCE@
t=/bin/toybox
$t mkdir -p /proc /sys /af-preboot
$t mount -t proc proc /proc
$t mount -t sysfs sysfs /sys
$t mknod /af-preboot/kmsg c 1 11
# The pinned firmware's userdata is partition 15, at LBA 0x01308000.
# Wait for kernel MMC discovery only; no uevent daemon or normal init is run.
i=0
while ! test -r /sys/class/block/mmcblk0p15/size; do
    test "$i" -lt 60
    i=$((i+1))
    $t sleep 1
done
check() {
    test "$($t cat /sys/class/block/mmcblk0/device/type)" = MMC
    test "$($t cat /sys/class/block/mmcblk0p15/dev)" = 179:15
    test "$($t cat /sys/class/block/mmcblk0p15/start)" = 19955712
    test "$($t cat /sys/class/block/mmcblk0p15/size)" -ge 2867200
    $t grep -qx PARTNAME=userdata /sys/class/block/mmcblk0p15/uevent
    # Refuse any mounted MMC partition (including aliases and bind mounts).
    if $t grep -q ' 179:' /proc/self/mountinfo; then return 1; fi
    for p in /sys/class/block/mmcblk0/holders/* /sys/class/block/mmcblk0p15/holders/*; do
        if test -e "$p"; then return 1; fi
    done
}
check
boot=$($t cat /proc/sys/kernel/random/boot_id)
cid=$($t sha256sum /sys/class/block/mmcblk0/device/cid)
cid=${cid%% *}
$t mknod /af-preboot/userdata b 179 15
hash=$($t dd if=/af-preboot/userdata bs=1048576 count=1468006400 iflag=count_bytes status=none | $t sha256sum)
hash=${hash%% *}
check
# A single small kernel-RAM record survives switch_root. No report is saved to
# userdata. The expected image digest is deliberately absent from this program.
printf '<6>ARKFORGE_PREBOOT_V1 %s %s %s 10217324544 1468006400 %s unmounted\n' "$nonce" "$boot" "$cid" "$hash" > /af-preboot/kmsg
