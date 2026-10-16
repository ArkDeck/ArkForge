//! Bounded Normal-mode reads. Caller paths, shell fragments and expected
//! digests never reach the device; the closed partition name comes from a plan.
use super::*;
use arkforge_ipc::contract;
use arkforge_ipc::messages::Effect;

pub(crate) const TARGETS: [&str; 9] = [
    "uboot",
    "resource",
    "boot_linux",
    "ramdisk",
    "system",
    "vendor",
    "updater",
    "chip_ckm",
    "userdata",
];
const MAX_BYTES: u64 = 16 * 1024 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PartitionRead {
    Hash(String),
    PrebootHash {
        digest: String,
        boot_id_sha256: String,
        storage_cid_sha256: String,
    },
    Skip(&'static str),
}

pub(crate) fn validate_range(effect: &Effect) -> Result<(), &'static str> {
    if effect.kind != "writePartition"
        || !TARGETS.contains(&effect.target.as_str())
        || effect.range_length == 0
        || effect.range_length > MAX_BYTES
        || !effect.range_start.is_multiple_of(512)
        || effect
            .range_start
            .checked_add(effect.range_length)
            .is_none()
        || Sha256Digest::parse_hex(&effect.content_sha256).is_err()
    {
        return Err("invalid-sealed-partition-range");
    }
    Ok(())
}

impl<R: CommandPort> HdcControlPort<R> {
    pub(crate) fn read_partition(
        &mut self,
        observation: &DeviceObservationView,
        effect: &Effect,
        deadline: Instant,
    ) -> Result<PartitionRead, &'static str> {
        validate_range(effect)?;
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
        let query = metadata_query(&effect.target);
        let before = self.partition_metadata(&key, &query, deadline)?;
        before.validate_range(effect)?;
        if before.writable {
            return Ok(PartitionRead::Skip(contract::SKIP_MOUNTED_WRITABLE));
        }
        let query_hash = hash_query(&before.name, effect.range_length);
        let bytes = self
            .runner
            .run(&["-t", &key, "shell", &query_hash], deadline)
            .map_err(|failure| failure.public_reason())?;
        let digest = parse_hash(&bytes)?;
        let after = self.partition_metadata(&key, &query, deadline)?;
        if before != after {
            return Err("partition-mapping-or-mount-changed");
        }
        Ok(PartitionRead::Hash(digest))
    }

    fn partition_metadata(
        &mut self,
        key: &str,
        query: &str,
        deadline: Instant,
    ) -> Result<BlockMetadata, &'static str> {
        let bytes = self
            .runner
            .run(&["-t", key, "shell", query], deadline)
            .map_err(|failure| failure.public_reason())?;
        parse_metadata(&bytes)
    }
}

fn metadata_query(target: &str) -> String {
    // target has been checked against TARGETS before this function is called.
    format!(
        r#"set -eu; p=$(/system/bin/toybox readlink -f /dev/block/by-name/{target}); case "$p" in /dev/block/mmcblk0p[0-9]|/dev/block/mmcblk0p[0-9][0-9]) ;; *) exit 31;; esac; test -b "$p"; n=${{p##*/}}; printf 'ARKFORGE_BLOCK_V1\n%s\n' "$n"; /system/bin/toybox cat /sys/class/block/$n/dev /sys/class/block/$n/start /sys/class/block/$n/size; /system/bin/toybox stat -c '%t:%T' "$p"; for h in /sys/class/block/$n/holders/* /sys/class/block/mmcblk0/holders/*; do if test -e "$h"; then printf 'HOLDER\n'; fi; done; printf 'MOUNTS\n'; /system/bin/toybox cat /proc/self/mountinfo; printf '\nARKFORGE_BLOCK_OK\n'"#
    )
}

fn hash_query(name: &str, bytes: u64) -> String {
    // count_bytes avoids head -c's signed 32-bit limit and covers partial blocks.
    // pipefail plus a terminal marker rejects a short/erroring remote pipeline,
    // even when the HDC client exits zero for a failed remote shell.
    format!(
        "set -eu; set -o pipefail; /system/bin/toybox dd if=/dev/block/{name} bs=1048576 count={bytes} iflag=count_bytes status=none | /system/bin/toybox sha256sum; printf '\\nARKFORGE_HASH_OK\\n'"
    )
}

#[derive(Debug, PartialEq, Eq)]
struct BlockMetadata {
    name: String,
    device: String,
    start: u64,
    size: u64,
    writable: bool,
}

impl BlockMetadata {
    fn validate_range(&self, effect: &Effect) -> Result<(), &'static str> {
        if self.start != effect.range_start || self.size < effect.range_length {
            return Err("partition-range-differs-from-sealed-plan");
        }
        Ok(())
    }
}

fn parse_metadata(bytes: &[u8]) -> Result<BlockMetadata, &'static str> {
    let invalid = "partition-metadata-invalid";
    if bytes.len() > OUTPUT_LIMIT {
        return Err(invalid);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid)?;
    let lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if lines.len() < 9
        || lines[0] != "ARKFORGE_BLOCK_V1"
        || lines.last() != Some(&"ARKFORGE_BLOCK_OK")
    {
        return Err(invalid);
    }
    let suffix = lines[1].strip_prefix("mmcblk0p").ok_or(invalid)?;
    if suffix.is_empty()
        || suffix.len() > 2
        || suffix.starts_with('0')
        || !suffix.bytes().all(|c| c.is_ascii_digit())
    {
        return Err(invalid);
    }
    let number = suffix.parse::<u64>().map_err(|_| invalid)?;
    // Linux MMC disk 0 and its first 31 partitions use major 179, minor 0..31.
    if number > 31 || lines[2] != format!("179:{number}") {
        return Err(invalid);
    }
    if lines[5] != format!("b3:{number:x}") {
        return Err("block-node-differs-from-sysfs");
    }
    let sectors = |s: &str| {
        s.parse::<u64>()
            .ok()
            .and_then(|n| n.checked_mul(512))
            .ok_or(invalid)
    };
    let mut metadata = BlockMetadata {
        name: lines[1].into(),
        device: lines[2].into(),
        start: sectors(lines[3])?,
        size: sectors(lines[4])?,
        writable: false,
    };
    if lines[6] == "HOLDER" {
        return Err("partition-has-block-holder");
    }
    if lines[6] != "MOUNTS" {
        return Err(invalid);
    }
    for line in &lines[7..lines.len() - 1] {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let separator = fields
            .iter()
            .position(|field| *field == "-")
            .ok_or(invalid)?;
        if separator < 6 || fields.len() != separator + 4 {
            return Err(invalid);
        }
        let options = [fields[5], fields[separator + 3]];
        for option in options {
            if !option.split(',').any(|v| v == "ro" || v == "rw") {
                return Err(invalid);
            }
        }
        if fields[2] == metadata.device || fields[2] == "179:0" {
            metadata.writable |= options.iter().any(|o| o.split(',').any(|v| v == "rw"));
        }
    }
    Ok(metadata)
}

fn parse_hash(bytes: &[u8]) -> Result<String, &'static str> {
    let invalid = "partition-hash-output-invalid";
    if bytes.len() > 256 {
        return Err(invalid);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid)?;
    let words = text.split_whitespace().collect::<Vec<_>>();
    if words.len() != 3
        || words[1] != "-"
        || words[2] != "ARKFORGE_HASH_OK"
        || words[0].len() != 64
        || !words[0]
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    {
        return Err(invalid);
    }
    Ok(words[0].into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    fn metadata(mounts: &str) -> Vec<u8> {
        format!("ARKFORGE_BLOCK_V1\nmmcblk0p7\n179:7\n245760\n4194304\nb3:7\nMOUNTS\n{mounts}\nARKFORGE_BLOCK_OK\n").into_bytes()
    }
    #[test]
    fn checks_offsets_capacity_bind_mounts_and_block_holders() {
        let ro = "1 0 179:7 / / ro,nodev - ext4 /dev/root ro,seclabel";
        let mut effect = Effect {
            kind: "writePartition".into(),
            target: "system".into(),
            range_start: 125829120,
            range_length: 2147483648,
            content_sha256: "a".repeat(64),
        };
        let parsed = parse_metadata(&metadata(ro)).unwrap();
        assert!(!parsed.writable);
        parsed.validate_range(&effect).unwrap();
        effect.range_start += 512;
        assert!(parsed.validate_range(&effect).is_err());
        effect.range_start -= 512;
        effect.range_length += 1;
        assert!(parsed.validate_range(&effect).is_err());
        for mount in [
            "2 1 179:7 /nested /bind ro - ext4 /dev/root rw",
            "2 1 179:0 / /whole rw - ext4 /dev/root rw",
        ] {
            assert!(
                parse_metadata(&metadata(&format!("{ro}\n{mount}")))
                    .unwrap()
                    .writable
            );
        }
        let holder = String::from_utf8(metadata(ro))
            .unwrap()
            .replace("MOUNTS", "HOLDER\nMOUNTS");
        assert_eq!(
            parse_metadata(holder.as_bytes()).unwrap_err(),
            "partition-has-block-holder"
        );
        for bad in ["mmcblk0p7;reboot", "dm-0", "mmcblk0p32"] {
            assert!(
                parse_metadata(
                    String::from_utf8(metadata(ro))
                        .unwrap()
                        .replace("mmcblk0p7", bad)
                        .as_bytes()
                )
                .is_err()
            );
        }
    }
    #[test]
    fn full_length_hash_requires_successful_remote_pipeline() {
        let digest = "a".repeat(64);
        assert_eq!(
            parse_hash(format!("{digest}  -\n\nARKFORGE_HASH_OK\n").as_bytes()).unwrap(),
            digest
        );
        for output in [
            format!("{digest} -"),
            format!("dd: I/O error\n{digest} -\nARKFORGE_HASH_OK"),
            format!("{} -\nARKFORGE_HASH_OK", "a".repeat(63)),
        ] {
            assert!(parse_hash(output.as_bytes()).is_err());
        }
        let query = hash_query("mmcblk0p7", 2147483648);
        assert!(query.contains("count=2147483648 iflag=count_bytes"));
        assert!(query.contains("set -o pipefail"));
    }

    #[derive(Debug)]
    struct Scripted {
        replies: VecDeque<Vec<u8>>,
        calls: Vec<Vec<String>>,
    }
    impl CommandPort for Scripted {
        fn run(&mut self, argv: &[&str], _: Instant) -> Result<Vec<u8>, ControlFailure> {
            self.calls
                .push(argv.iter().map(|v| v.to_string()).collect());
            Ok(self
                .replies
                .pop_front()
                .expect("unexpected extra device command"))
        }
    }
    fn port(replies: Vec<Vec<u8>>) -> (HdcControlPort<Scripted>, DeviceObservationView, Effect) {
        let observation = DeviceObservationView {
            observation_id: "OBS-TEST".into(),
            mode: "hdc-normal".into(),
            topology_sha256: "topology".into(),
            serial_sha256: digest_in_domain(Domain::DeviceSerial, b"exact-target").to_hex(),
            observed_at_epoch_ms: 1,
            descriptor_sha256: "descriptor".into(),
            identity_strength: "serialAndTopology".into(),
            malformed_descriptor: false,
            protocol_identity: Vec::new(),
            serial_evidence_kind: "usbDescriptor".into(),
        };
        let mut all =
            VecDeque::from([b"exact-target\t\tUSB\tConnected\tlocalhost\thdc\n".to_vec()]);
        all.extend(replies);
        (
            HdcControlPort {
                runner: Scripted {
                    replies: all,
                    calls: Vec::new(),
                },
                sessions: BTreeMap::new(),
                resolve_native_usb_target: false,
            },
            observation,
            Effect {
                kind: "writePartition".into(),
                target: "system".into(),
                range_start: 125829120,
                range_length: 2147483648,
                content_sha256: "a".repeat(64),
            },
        )
    }

    #[test]
    fn read_uses_exact_target_full_bytes_and_checks_the_mapping_twice() {
        let ro = metadata("1 0 179:7 / / ro,nodev - ext4 /dev/root ro,seclabel");
        let hash = format!("{}  -\nARKFORGE_HASH_OK\n", "a".repeat(64)).into_bytes();
        let (mut port, observation, effect) = port(vec![ro.clone(), hash, ro]);
        assert_eq!(
            port.read_partition(
                &observation,
                &effect,
                Instant::now() + Duration::from_secs(1)
            ),
            Ok(PartitionRead::Hash("a".repeat(64)))
        );
        assert!(port.runner.replies.is_empty());
        for argv in &port.runner.calls[1..] {
            assert_eq!(&argv[..3], ["-t", "exact-target", "shell"]);
            assert!(!argv[3].contains(&effect.content_sha256));
        }
        assert_eq!(port.runner.calls[1], port.runner.calls[3]);
        assert!(port.runner.calls[2][3].contains("count=2147483648 iflag=count_bytes"));
        assert!(!port.runner.calls[2][3].contains("by-name"));
    }

    #[test]
    fn writable_or_misbound_partition_never_reaches_the_hash_command() {
        let rw = metadata("1 0 179:7 / / rw,nodev - ext4 /dev/root rw,seclabel");
        let (mut reader, observation, effect) = port(vec![rw]);
        assert_eq!(
            reader.read_partition(
                &observation,
                &effect,
                Instant::now() + Duration::from_secs(1)
            ),
            Ok(PartitionRead::Skip("mounted-writable-after-boot"))
        );
        assert_eq!(reader.runner.calls.len(), 2);
        let ro = metadata("1 0 179:7 / / ro - ext4 /dev/root ro");
        for bytes in [
            String::from_utf8(ro.clone())
                .unwrap()
                .replace("245760", "245761")
                .into_bytes(),
            String::from_utf8(ro)
                .unwrap()
                .replace("b3:7", "b3:8")
                .into_bytes(),
        ] {
            let (mut reader, observation, effect) = port(vec![bytes]);
            assert!(
                reader
                    .read_partition(
                        &observation,
                        &effect,
                        Instant::now() + Duration::from_secs(1)
                    )
                    .is_err()
            );
            assert_eq!(reader.runner.calls.len(), 2);
        }
    }

    #[test]
    fn a_mount_change_or_invalid_range_never_produces_a_hash_result() {
        let ro = metadata("1 0 179:7 / / ro - ext4 /dev/root ro");
        let rw = metadata("1 0 179:7 / / rw - ext4 /dev/root rw");
        let hash = format!("{}  -\nARKFORGE_HASH_OK\n", "a".repeat(64)).into_bytes();
        let (mut reader, observation, effect) = port(vec![ro, hash, rw]);
        assert_eq!(
            reader.read_partition(
                &observation,
                &effect,
                Instant::now() + Duration::from_secs(1)
            ),
            Err("partition-mapping-or-mount-changed")
        );
        for target in ["../../userdata", "misc", "system;reboot"] {
            let (mut reader, observation, mut effect) = port(vec![]);
            effect.target = target.into();
            assert!(
                reader
                    .read_partition(&observation, &effect, Instant::now())
                    .is_err()
            );
            assert!(reader.runner.calls.is_empty());
        }
    }
}
