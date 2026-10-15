#!/bin/sh
# PID 1 remains in the initramfs until the bounded, read-only measurement ends.
# Never mount, fsck, format or repair a persistent filesystem here.
/bin/toybox timeout 300 /bin/sh /arkforge-preboot
# Only the two virtual filesystems mounted by the measurement are removed.
/bin/toybox umount /proc
/bin/toybox umount /sys
exec /bin/init_early "$@"
