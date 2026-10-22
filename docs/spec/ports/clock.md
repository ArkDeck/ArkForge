# Port: clocks
status: normative
source: crates/arkforge-authority-api/src/lib.rs evaluate_freshness; crates/arkforged/src/jobs.rs

## Purpose
Say which timestamps are wall-clock and which decisions may depend on them.

## Rules
- Every `*EpochMs` field (plan creation/expiry, permit issue/expiry, snapshot
  capture/deadline, journal `atEpochMs`, transcript times) is **Unix epoch
  milliseconds, wall clock, unsigned 64-bit**.
- Expiry comparisons are `now >= expiresAt` (exclusive end) everywhere
  (AF-AUTH-016, AF-PLAN-023, AF-AUTH-004).
- The wall clock is a *backstop*, never the primary fact: admission freshness
  is continuity first (AF-AUTH-003); a stale clock result is retried without
  blaming the device.
- Deadlines for waits (rebind tolerance window, control request deadline,
  snapshot lifetime) are per-step budgets carried in the data, not global
  constants; an implementation MUST NOT tighten them silently.
- Monotonic time MAY be used internally for timeouts; it never appears on the
  wire or in a digest.
- The daemon never sets the clock and never trusts a peer's timestamp for its
  own decisions: `now` is the daemon's clock at the moment of the check.

## Error classes
None; a clock is infallible. An implementation whose clock goes backwards MUST
still compare with the rules above (saturating subtraction for elapsed times).
