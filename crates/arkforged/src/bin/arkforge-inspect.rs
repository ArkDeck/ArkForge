//! `arkforge-inspect` — import a real firmware archive and report its manifest.
//!
//! Read-only, offline diagnostics (architecture.md 15.1). It imports into the
//! content store, hashes every byte on the way in, and prints what the parser
//! could establish — including what it could not.
//!
//! The store is the only thing that ever reads the archive twice: the parser
//! reads the stored object, never the path the caller named
//! (architecture.md 10.1).

use arkforge_artifact::cas::{CasQuota, ContentAddressedStore};
use arkforge_artifact::dayu200;
use arkforge_core::profile;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(()) => {}
        Err(message) => {
            eprintln!("arkforge-inspect: {message}");
            std::process::exit(1);
        }
    }
}

fn usage() -> String {
    concat!(
        "usage: arkforge-inspect --archive <file> --store <dir> [--profile <file>]\n",
        "\n",
        "  --archive  the firmware archive to import and inspect\n",
        "  --store    content-addressed store root\n",
        "  --profile  compare the manifest against a DeviceProfile\n",
        "\n",
        "Read-only. Nothing is written to a device.\n"
    )
    .to_string()
}

fn run(arguments: &[String]) -> Result<(), String> {
    let mut archive: Option<PathBuf> = None;
    let mut store_root: Option<PathBuf> = None;
    let mut profile_path: Option<PathBuf> = None;

    let mut index = 0usize;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--archive" => {
                index += 1;
                archive = Some(PathBuf::from(arguments.get(index).ok_or_else(usage)?));
            }
            "--store" => {
                index += 1;
                store_root = Some(PathBuf::from(arguments.get(index).ok_or_else(usage)?));
            }
            "--profile" => {
                index += 1;
                profile_path = Some(PathBuf::from(arguments.get(index).ok_or_else(usage)?));
            }
            "--help" | "-h" => {
                print!("{}", usage());
                return Ok(());
            }
            other => return Err(format!("unexpected argument {other:?}\n\n{}", usage())),
        }
        index += 1;
    }

    let archive = archive.ok_or_else(usage)?;
    let store_root = store_root.ok_or_else(usage)?;

    let store = ContentAddressedStore::open(&store_root, CasQuota::dayu200_default())
        .map_err(|error| error.to_string())?;

    let size = std::fs::metadata(&archive)
        .map_err(|error| format!("{}: {error}", archive.display()))?
        .len();
    println!("archive   {}", archive.display());
    println!("size      {size} bytes");

    let report = store.preflight(size).map_err(|error| error.to_string())?;
    println!(
        "preflight {} (store holds {}, volume free {})",
        if report.accepted { "accepted" } else { "REFUSED" },
        report.store_bytes_in_use,
        report.volume_available_bytes
    );
    if !report.accepted {
        return Err(report.blocker.unwrap_or_else(|| "refused".into()));
    }

    let file = std::fs::File::open(&archive).map_err(|error| error.to_string())?;
    let started = Instant::now();
    let imported = store
        .import(file, size, None)
        .map_err(|error| error.to_string())?;
    let import_seconds = started.elapsed().as_secs_f64();
    println!(
        "import    {:.2}s ({:.1} MiB/s){}",
        import_seconds,
        size as f64 / 1_048_576.0 / import_seconds,
        if imported.deduplicated {
            " [deduplicated]"
        } else {
            ""
        }
    );
    println!("sha256    {}", imported.digest);

    let started = Instant::now();
    let manifest = dayu200::inspect(
        store
            .open_object(&imported.digest)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    println!("inspect   {:.2}s", started.elapsed().as_secs_f64());
    println!(
        "manifest  {}",
        manifest.digest().map_err(|error| error.to_string())?
    );
    println!("format    {}", manifest.format.id);
    println!("confidence {}", manifest.confidence.as_str());

    println!("\nmembers ({})", manifest.members.len());
    for member in &manifest.members {
        println!(
            "  {:<20} {:>12}  {}  {}",
            member.path,
            member.size_bytes,
            member.sha256,
            member.role.as_str()
        );
    }

    if let Some(table) = &manifest.partition_table {
        println!("\npartitions ({}) device={}", table.entries.len(), table.device);
        for entry in &table.entries {
            let extent = match entry.size_sectors {
                Some(size) => format!("{size} sectors"),
                None => "remainder".to_string(),
            };
            println!(
                "  {:>2} {:<14} @{:<10} {:<16} {}",
                entry.index,
                entry.name,
                entry.offset_sectors,
                extent,
                entry.grammar_branch.as_str()
            );
        }
    }

    println!("\nbuild facts");
    if manifest.build_facts.is_empty() {
        println!("  (none found inside any hashed image member)");
    }
    for (key, value) in &manifest.build_facts {
        println!("  {key} = {value}");
    }

    if !manifest.unclassified_members.is_empty() {
        println!("\nunclassified members");
        for member in &manifest.unclassified_members {
            println!("  {member}");
        }
    }
    if !manifest.execution_relevant_unknowns.is_empty() {
        println!("\nexecution-relevant unknowns");
        for unknown in &manifest.execution_relevant_unknowns {
            println!("  {} — {}", unknown.id, unknown.summary);
        }
    }

    if let Some(path) = profile_path {
        let source = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        let profile = profile::load(&source).map_err(|error| error.to_string())?;
        println!("\nprofile {} target coverage", profile.id);
        let mut ordered: Vec<_> = profile.allowed_targets.iter().collect();
        ordered.sort_by_key(|target| target.write_order);
        for target in ordered {
            let member = target
                .source_member
                .as_deref()
                .and_then(|name| manifest.member(name));
            match member {
                Some(member) => println!(
                    "  {:>1} {:<12} <- {:<18} {:>12} bytes  present",
                    target.write_order,
                    target.partition,
                    member.path,
                    member.size_bytes
                ),
                None => println!(
                    "  {:>1} {:<12} <- {:<18} MISSING",
                    target.write_order,
                    target.partition,
                    target.source_member.as_deref().unwrap_or("-")
                ),
            }
        }
    }

    Ok(())
}
