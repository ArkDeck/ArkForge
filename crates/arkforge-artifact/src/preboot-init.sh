#!/bin/sh
# PID 1 remains in the initramfs until the bounded, read-only measurement ends.
# Never mount, fsck, format or repair a persistent filesystem here.
t=/bin/toybox
$t mkdir -p /af-preboot
original_umask=$(umask)
umask 077
# TERM can remain ignored while the inherited shell waits for a pipeline.
# The read-only measurement has no persistent cleanup to defer before KILL.
$t timeout -s KILL 300 /bin/sh /arkforge-preboot > /af-preboot/measure.log 2>&1
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
    # The pinned toybox has od but no tr. Read one byte beyond the complete
    # NUL-delimited argv so a matching prefix with extra arguments is refused.
    # Stock init treats an existing /dev/null as proof that /dev is mounted.
    # It does not exist yet; redirecting there would create a regular file and
    # make stock init skip the tmpfs that must survive its root switch.
    bytes=$($t od -An -v -tx1 -N 30 /proc/$holder/cmdline 2>>/af-preboot/measure.log)
    command=
    for byte in $bytes; do command=$command$byte; done
    if test "$command" = 2f61662d707265626f6f742f746f79626f7800736c6565700036303000; then break; fi
    i=$((i+1))
    $t sleep 1
done
# Only the two virtual filesystems mounted by the measurement are removed.
/bin/toybox umount /proc
/bin/toybox umount /sys
umask "$original_umask"
exec /bin/init_early "$@"
