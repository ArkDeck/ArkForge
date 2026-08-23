# Port: file system (runtime directory, content-addressed store)
status: draft
source: crates/arkforge-artifact/src/cas.rs, crates/arkforge-artifact/src/staging.rs, crates/arkforged/src/main.rs

## Purpose
Hold the journal files, the content-addressed artifact store and the staged
images, with crash-safe replacement and no dependence on caller paths.

## Operations
| op | notes |
|---|---|
| `available_bytes(path)` | volume free space, used by the import preflight (`QuotaExceeded` reports the preflight) |
| `import(stream, expectedSize, expectedSha256?)` | streams to a temporary object, hashes while copying, then renames to `<sha256>`; a mismatch or overrun refuses and leaves no object |
| `open_object(sha256)` | read-only handle on an immutable object |
| `acquire_lease(sha256, holder)` / `release_lease` | a leased object is never evicted |
| `stage(manifest, member)` | writes a member to a staging directory under a name derived from the manifest, refusing names that escape the directory |

## Ownership and lifetime
Objects are immutable once named; the store owns them; leases are named by the
plan/job that holds them and survive restarts as files.

## Thread-safety
Imports of different digests may run concurrently; the same digest is
deduplicated by the final rename.

## Crash / retry
An interrupted import leaves only a temporary file, never a partially written
object under its final name; reopening the store ignores temporaries. Rename
is the atomic commit on every supported platform.

## Security
The daemon MUST NOT open a caller-supplied path for a destructive plan
(AF-ART-001); staged names MUST NOT contain path separators or traversal.

## Error classes
`ARTIFACT_STORE_FAILED`, `ARTIFACT_IMPORT_REFUSED`, `ARTIFACT_REJECTED`,
`ARTIFACT_NOT_FOUND`, `ARTIFACT_FILE_NOT_FOUND`, and the `ARC0xx` parser codes.
