# `spec/ports/`

A *port* is a boundary between ArkForge's language-neutral core and something
it cannot specify by bytes alone: an operating system, a USB stack, a clock, a
file system, another process. Each contract states what the boundary must
promise so that Zig (allocator + error union), C++ (RAII, `std::span`,
`std::expected`) and Rust can each implement it without reading each other.

Every port file covers, in order: purpose; operations (abstract inputs and
outputs); ownership and lifetime; thread-safety; deadlines and clocks; short
reads/writes; idempotency; external effects before/after each call;
crash/retry; error classes (stable names, never OS error text); conformance
hooks (what a mock/replay must be able to do).

| file | port | status |
|---|---|---|
| `durability.md` | stable storage for the journal (fsync semantics) | normative for the guarantee, informative for platform notes |
| `clock.md` | wall clock vs monotonic time | normative |
| `filesystem.md` | runtime directory, CAS, atomic file replacement | draft |
| `usb.md` | bulk USB interface claim/transfer | draft + informative RockUSB appendix |
| `transport-identity.md` | observation, open-exact, rebind | draft |
| `device-control.md` | managed HDC control port | draft |
| `ipc-framing.md` | local socket/pipe endpoint and framing | draft |

Language mappings (`mappings/<lang>.yaml`) say how each port's error classes
and ownership rules land in a given language; they may not change the contract.
