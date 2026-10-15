#!/bin/sh
# PID 1 remains in the initramfs until the bounded, read-only measurement ends.
# Never mount, fsck, format or repair a persistent filesystem here.
t=/bin/toybox
$t mkdir -p /af-preboot
original_umask=$(umask)
umask 077
$t timeout 300 /bin/sh /arkforge-preboot > /af-preboot/measure.log 2>&1
result=$?
if test "$result" != 0 || ! test -s /af-preboot/report; then
    printf 'ARKFORGE_PREBOOT_FAILED_V1 %s\n' "$result" > /af-preboot/report
fi
$t chmod 400 /af-preboot/report /af-preboot/measure.log
# switch_root may unlink the old rootfs, and normal boot can evict early kmsg.
# An open read-only FD keeps the small RAM file alive without any disk mount.
# The holder only sleeps, exits after ten minutes and has no write descriptor.
$t ln -s /bin/toybox /af-preboot/toybox
/af-preboot/toybox sleep 600 9</af-preboot/report 8</af-preboot/measure.log 0<&- 1>&- 2>&- &
holder=$!
i=0
while test "$i" -lt 5; do
    command=$($t tr '\000' ' ' < /proc/$holder/cmdline)
    if test "$command" = '/af-preboot/toybox sleep 600 '; then break; fi
    i=$((i+1))
    $t sleep 1
done
# Only the two virtual filesystems mounted by the measurement are removed.
/bin/toybox umount /proc
/bin/toybox umount /sys
umask "$original_umask"
exec /bin/init_early "$@"
