//! Read the kernel-RAM report of the closed acceptance initramfs. No expected
//! digest or caller-controlled text is sent to the device.
use super::*;
use crate::hdc_control::partition::PartitionRead;
use arkforge_artifact::preboot::{USERDATA_BYTES, USERDATA_START};

const QUERY: &str = r#"set -eu; set -o pipefail; printf 'ARKFORGE_PREBOOT_READ_V1\n'; /system/bin/toybox cat /proc/sys/kernel/random/boot_id; /system/bin/toybox sha256sum /sys/class/block/mmcblk0/device/cid; /system/bin/toybox dmesg | /system/bin/toybox grep -F ARKFORGE_PREBOOT_V1; printf '\nARKFORGE_PREBOOT_READ_OK\n'"#;

impl<R: CommandPort> HdcControlPort<R> {
    pub(crate) fn read_preboot(
        &mut self,
        observation: &DeviceObservationView,
        nonce: &str,
        deadline: Instant,
    ) -> Result<PartitionRead, &'static str> {
        if observation.mode != "hdc-normal" {
            return Err("normal-mode-required");
        }
        let key = self
            .exact_target(
                &observation.serial_sha256,
                &observation.topology_sha256,
                deadline,
            )
            .map_err(|failure| failure.public_reason())?;
        let bytes = self
            .runner
            .run(&["-t", &key, "shell", QUERY], deadline)
            .map_err(|failure| failure.public_reason())?;
        parse(&bytes, nonce)
    }
}

fn lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn parse(bytes: &[u8], nonce: &str) -> Result<PartitionRead, &'static str> {
    let invalid = "preboot-report-invalid-or-stale";
    if bytes.len() > 4096 || !lower_hex(nonce, 32) {
        return Err(invalid);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid)?;
    let lines = text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    let [begin, boot, cid_line, report, end] = lines.as_slice() else {
        return Err(invalid);
    };
    if *begin != "ARKFORGE_PREBOOT_READ_V1"
        || *end != "ARKFORGE_PREBOOT_READ_OK"
        || boot.len() != 36
        || !lower_hex(&boot.replace('-', ""), 32)
    {
        return Err(invalid);
    }
    let cid = cid_line.split_whitespace().collect::<Vec<_>>();
    if cid.len() != 2 || !lower_hex(cid[0], 64) || cid[1] != "/sys/class/block/mmcblk0/device/cid" {
        return Err(invalid);
    }
    // dmesg owns the timestamp prefix. Require exactly one record and every
    // field; the record has no caller-controlled strings beyond a hex nonce.
    let (_, body) = report.split_once("ARKFORGE_PREBOOT_V1 ").ok_or(invalid)?;
    let words = body.split_whitespace().collect::<Vec<_>>();
    if words.len() != 7
        || words[0] != nonce
        || words[1] != *boot
        || words[2] != cid[0]
        || words[3] != USERDATA_START.to_string()
        || words[4] != USERDATA_BYTES.to_string()
        || !lower_hex(words[5], 64)
        || words[6] != "unmounted"
    {
        return Err(invalid);
    }
    Ok(PartitionRead::PrebootHash {
        digest: words[5].into(),
        boot_id_sha256: arkforge_core::digest::sha256(boot.as_bytes()).to_hex(),
        storage_cid_sha256: cid[0].into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn output(nonce: &str, report_boot: &str) -> String {
        format!(
            "ARKFORGE_PREBOOT_READ_V1\n01234567-89ab-cdef-0123-456789abcdef\n{}  /sys/class/block/mmcblk0/device/cid\n[  12.34] ARKFORGE_PREBOOT_V1 {nonce} {report_boot} {} {USERDATA_START} {USERDATA_BYTES} {} unmounted\nARKFORGE_PREBOOT_READ_OK\n",
            "aa".repeat(32),
            "aa".repeat(32),
            "bb".repeat(32)
        )
    }
    #[test]
    fn binds_nonce_boot_storage_range_and_exactly_one_complete_report() {
        let nonce = "0123456789abcdef0123456789abcdef";
        let good = output(nonce, "01234567-89ab-cdef-0123-456789abcdef");
        assert!(
            matches!(parse(good.as_bytes(), nonce).unwrap(), PartitionRead::PrebootHash { digest, .. } if digest == "bb".repeat(32))
        );
        for bad in [
            good.replace(nonce, &"0".repeat(32)),
            good.replace("unmounted", "mounted"),
            good.replace(&USERDATA_BYTES.to_string(), "512"),
            good.replace("ARKFORGE_PREBOOT_READ_OK", ""),
            format!("{good}{good}"),
            output(nonce, "00000000-0000-0000-0000-000000000000"),
            good.replacen(&"aa".repeat(32), &"cc".repeat(32), 1),
        ] {
            assert!(parse(bad.as_bytes(), nonce).is_err());
        }
    }
}
