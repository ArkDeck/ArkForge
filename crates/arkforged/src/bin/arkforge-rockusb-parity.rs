//! NRU-001 read-only A/B harness.
//!
//! Both backends read the same attached Loader sequentially.  The report stores
//! hashes and descriptor digests, never raw serials or sector contents.

use arkforge_core::digest::sha256;
use arkforge_provider::rockchip_execute::{RockUsbLocation, RockUsbPort};
use arkforge_transport::usb::{IoRegEnumerator, UsbEnumerator};
use arkforged::dispatch::{NativeRockUsbPort, VendorToolPort};
use std::path::PathBuf;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = run(&arguments) {
        eprintln!("arkforge-rockusb-parity: {error}");
        std::process::exit(1);
    }
}

fn usage() -> String {
    concat!(
        "usage: arkforge-rockusb-parity --rkdeveloptool <absolute-path> ",
        "--rkdeveloptool-sha256 <digest> --output-dir <directory>\n",
        "\n",
        "Read-only: compares native and vendor discovery, GPT semantics, the first sector, ",
        "primary/backup GPT windows, and three deterministic random LBA windows.\n"
    )
    .into()
}

fn run(arguments: &[String]) -> Result<(), String> {
    let mut tool_path = None;
    let mut expected_digest = None;
    let mut output_dir = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--rkdeveloptool" => {
                index += 1;
                tool_path = Some(PathBuf::from(arguments.get(index).ok_or_else(usage)?));
            }
            "--rkdeveloptool-sha256" => {
                index += 1;
                expected_digest = Some(arguments.get(index).ok_or_else(usage)?.clone());
            }
            "--output-dir" => {
                index += 1;
                output_dir = Some(PathBuf::from(arguments.get(index).ok_or_else(usage)?));
            }
            "--help" | "-h" => {
                print!("{}", usage());
                return Ok(());
            }
            other => return Err(format!("unknown argument {other:?}\n\n{}", usage())),
        }
        index += 1;
    }

    let tool_path = tool_path.ok_or_else(usage)?;
    let expected = arkforge_core::Sha256Digest::parse_hex(&expected_digest.ok_or_else(usage)?)
        .map_err(|error| format!("--rkdeveloptool-sha256: {error}"))?;
    let output_dir = output_dir.ok_or_else(usage)?;
    std::fs::create_dir_all(&output_dir)
        .map_err(|error| format!("{}: {error}", output_dir.display()))?;
    let scratch = output_dir.join("scratch");
    std::fs::create_dir_all(&scratch).map_err(|error| format!("{}: {error}", scratch.display()))?;

    let vendor = VendorToolPort::open(&tool_path)?;
    if vendor.digest() != expected {
        return Err(format!(
            "{} hashes to {}, expected {expected}",
            tool_path.display(),
            vendor.digest()
        ));
    }
    vendor
        .self_test(&["-v"], "rkdeveloptool", std::time::Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    let native = NativeRockUsbPort::new();

    let native_devices = native.discover().map_err(|error| error.to_string())?;
    let vendor_devices = vendor.discover().map_err(|error| error.to_string())?;
    let native_device = only("native", &native_devices.value)?;
    let vendor_device = only("vendor", &vendor_devices.value)?;
    if (
        native_device.vendor_id,
        native_device.product_id,
        native_device.mode.as_str(),
    ) != (
        vendor_device.vendor_id,
        vendor_device.product_id,
        vendor_device.mode.as_str(),
    ) {
        return Err(format!(
            "discovery differs: native={} vendor={}",
            native_device.summary(),
            vendor_device.summary()
        ));
    }

    let native_topology = match native_device.location {
        RockUsbLocation::IokitTopology(value) => value,
        RockUsbLocation::VendorBusPort(_) => {
            return Err("native discovery mislabeled a vendor location as IOKit topology".into())
        }
    };
    let vendor_bus_port = match vendor_device.location {
        RockUsbLocation::VendorBusPort(value) => value,
        RockUsbLocation::IokitTopology(_) => {
            return Err("vendor discovery mislabeled IOKit topology as its LocationID".into())
        }
    };

    let ioreg = IoRegEnumerator
        .enumerate()
        .map_err(|error| format!("ioreg cross-check: {error}"))?;
    let os_record = ioreg
        .iter()
        .find(|record| {
            record.vendor_id == native_device.vendor_id
                && record.product_id == native_device.product_id
                && record.location_id == native_topology
        })
        .ok_or_else(|| "ioreg has no descriptor matching native discovery".to_string())?;
    if native_device.serial.as_deref() != os_record.serial.as_deref() {
        return Err("native serial descriptor differs from the independent ioreg view".into());
    }

    let native_table = native
        .read_partition_table()
        .map_err(|error| error.to_string())?;
    let vendor_table = vendor
        .read_partition_table()
        .map_err(|error| error.to_string())?;
    if native_table.value != vendor_table.value {
        return Err(format!(
            "partition semantics differ:\nnative={:#?}\nvendor={:#?}",
            native_table.value, vendor_table.value
        ));
    }

    let capacity = native
        .read_capacity_sectors()
        .map_err(|error| error.to_string())?;
    if capacity < 128 {
        return Err(format!("device reports only {capacity} sectors"));
    }
    let mut windows = vec![(0, 1), (1, 33), (capacity - 33, 33)];
    windows.extend(random_windows(capacity));

    let mut report = String::new();
    report.push_str("TASK-NRU-001 native/vendor read parity\n");
    report.push_str("destructive dispatch = 0\n");
    report.push_str(&format!("rkdeveloptool_sha256={}\n", vendor.digest()));
    report.push_str(&format!("iokit_topology={native_topology:08x}\n"));
    report.push_str(&format!("vendor_bus_port={vendor_bus_port:x}\n"));
    report.push_str(&format!(
        "bcd_usb={:04x} mode_bit=loader\n",
        native_device.usb_specification.unwrap_or(0)
    ));
    report.push_str(&format!(
        "serial_digest={}\n",
        sha256(native_device.serial.as_deref().unwrap_or("").as_bytes())
    ));
    report.push_str(&format!("capacity_sectors={capacity}\n"));
    report.push_str(&format!(
        "partition_count={} partition_semantics=equal\n",
        native_table.value.entries.len()
    ));

    for (begin, sectors) in windows {
        let native_read = native
            .read_sectors(begin, sectors, &scratch)
            .map_err(|error| format!("native {begin}+{sectors}: {error}"))?;
        let vendor_read = vendor
            .read_sectors(begin, sectors, &scratch)
            .map_err(|error| format!("vendor {begin}+{sectors}: {error}"))?;
        if native_read.value != vendor_read.value {
            return Err(format!(
                "read differs at {begin}+{sectors}: native={} vendor={}",
                sha256(&native_read.value),
                sha256(&vendor_read.value)
            ));
        }
        report.push_str(&format!(
            "window={begin}+{sectors} bytes={} sha256={} parity=equal\n",
            native_read.value.len(),
            sha256(&native_read.value)
        ));
    }

    let report_path = output_dir.join("nru-001-read-parity.txt");
    std::fs::write(&report_path, report.as_bytes())
        .map_err(|error| format!("{}: {error}", report_path.display()))?;
    let _ = std::fs::remove_dir(&scratch);
    println!("PASS {}", report_path.display());
    Ok(())
}

fn only<'a>(
    label: &str,
    devices: &'a [arkforge_provider::rockchip_execute::RockUsbDevice],
) -> Result<&'a arkforge_provider::rockchip_execute::RockUsbDevice, String> {
    if devices.len() != 1 {
        return Err(format!(
            "{label} discovery returned {} devices, expected one",
            devices.len()
        ));
    }
    Ok(&devices[0])
}

fn random_windows(capacity: u64) -> Vec<(u64, u64)> {
    // Reproducible xorshift seed derived from the device's own capacity. These
    // are random LBA windows without making an evidence run irreproducible.
    let mut state = capacity ^ 0x4e52_552d_3030_31;
    let span = capacity.saturating_sub(64).max(1);
    (0..3)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (32 + state % span, 8)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_windows_are_reproducible_and_bounded() {
        let capacity = 31_250_000;
        let first = random_windows(capacity);
        assert_eq!(first, random_windows(capacity));
        assert_eq!(first.len(), 3);
        assert!(first
            .iter()
            .all(|(begin, sectors)| *begin + *sectors <= capacity));
    }
}
