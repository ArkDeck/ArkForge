# Design — unified `arkforge` command surface

## 0. Execution shape

~~~text
operator or Agent
      │
      ▼
arkforge command frontend
      │ owner-only local IPC
      ▼
CLI authority supervisor ── public/controller IPC ──▶ arkforged mechanics
      │                                                  │
      ├─ local target binding                            ├─ CAS + parser
      ├─ exact StepPermit                                ├─ native RockUSB
      ├─ typed HDC managed control                       └─ durable job journal
      └─ authority decision/permit journal

explicit rescue only
      │
      └─ RescuePlan ── NativeRockUsbPort ──▶ RescueReceipt
          (shared native protocol, separate evidence/authority domain)
~~~

`arkforge` becoming a direct-flash CLI does not turn it into a one-shot mechanics
process. `arkforge daemon start` launches a persistent local authority supervisor
beside `arkforged`. The supervisor keeps the pairing secret in memory and the daemon
keeps USB session continuity, durable jobs and no-replay recovery. The short-lived
command process is presentation and intent submission, not the secret owner.

## 1. Command grammar

### 1.1 Naming rules

- One public binary: `arkforge`; the daemon remains `arkforged`.
- Groups are singular nouns: `device`, `artifact`, `job`, `daemon`, `signing`.
- Collection actions are always `list`; one resource is always `show`.
- Commands and long options use lowercase kebab-case.
- Resource options use one vocabulary everywhere:
  `--device`, `--artifact`, `--profile`, `--plan`, `--job`.
- A local file is never overloaded with an object ID: use `--file`, `--image` or
  `--out`. The parser does not guess whether a positional is a path or an ID.
- Destructive work is always `plan` followed by `apply`. `run`, `execute`, `go`,
  `yes` and `force` are not aliases.
- Options do not change meaning between commands. In particular,
  `--expect-*-sha256` always checks caller expectation; it never merely prints a digest.

### 1.2 Global options

~~~text
Global options:
  --runtime-dir <dir>             Runtime state and local sockets; default:
                                  the platform per-user ArkForge state directory
  --output <human|json|jsonl>     Output format; default: human
  --no-color                      Disable ANSI styling in human output
  --quiet                         Print only the final human result
  --verbose                       Include diagnostic evidence; never include secrets
  -h, --help                      Show help for the current command
  -V, --version                   Show version and protocol support
~~~

`--output json` and `--output jsonl` imply `--no-color`. No command prompts for
input. Missing acknowledgement is a typed refusal, not a TTY question. This keeps
the same invocation deterministic in a terminal, CI and an Agent process.

`--runtime-dir` identifies a complete local runtime, not an arbitrary socket.
The client chooses `public.sock` or `controller.sock` from the requested operation.
An advanced caller cannot make `flash apply` use the public socket or make a read
command acquire controller authority.

## 2. Top-level help

The intended text is deliberately workflow-oriented:

~~~text
ArkForge plans, executes, verifies, and recovers device firmware operations.

Usage:
  arkforge [global options] <command> [<subcommand>] [options]

Start a normal flash:
  1. arkforge doctor
  2. arkforge daemon start
  3. arkforge device list
  4. arkforge artifact import --file <firmware>
  5. arkforge flash assess --artifact <id> --profile <id@version> \
       --device <observation-id> --intent full-restore
  6. arkforge flash plan --artifact <id> --profile <id@version> \
       --device <observation-id> --intent full-restore
  7. arkforge flash apply --plan <id> --expect-plan-sha256 <sha256> \
       --ack <token> [--ack <token>...]

Commands:
  doctor       Check whether this host can inspect or execute
  device       List, inspect, and probe device observations
  artifact     Import and inspect firmware artifacts
  flash        Assess, plan, and apply a normal flash
  job          Observe, cancel, reconcile, and recover durable jobs
  rescue       Perform an explicit recovery operation outside normal flash
  daemon       Run and manage the local mechanics daemon
  signing      Verify the macOS binary signing contract
  completion   Generate shell completion
  help         Describe commands for humans or Agents

Execution model:
  Normal flash uses an exact plan and per-step permits. Rescue is never selected
  automatically and never produces a normal-flash success receipt.

Agent discovery:
  arkforge help --format json
  arkforge help flash plan --format json

Run 'arkforge <command> --help' for human-readable command help.
~~~

## 3. Complete command inventory

| Command | Effect | Result and next step |
|---|---|---|
| `doctor` | read-only | Host checks; points to daemon/profile/tool remediation |
| `device list` | read-only | Observations; next `device probe` or `flash assess` |
| `device show` | read-only | One stored observation with identity strength |
| `device probe` | read-only | Provider candidates and capabilities |
| `device wait` | read-only | Waits for an exact profile/mode; refuses ambiguity |
| `artifact import` | host write only | CAS artifact ID/hash; next `artifact inspect` |
| `artifact inspect` | read-only | Parsed manifest/profile compatibility |
| `artifact list` | read-only | Imported artifacts |
| `artifact show` | read-only | Stored manifest by artifact ID |
| `flash assess` | read-only | Always an assessment, never an executable plan |
| `flash plan` | read-only device access + host write | Sealed executable plan or typed unavailable result |
| `flash apply` | destructive | Starts and drives a permitted durable job |
| `job list/show/watch` | read-only | Durable state and receipts |
| `job cancel` | mutating control | Queues cancellation at the declared boundary |
| `job reconcile` | read-only | Possible-effect assessment; never replays work |
| `job recovery guide` | read-only | Typed manual/superseding recovery guidance |
| `job recovery plan` | read-only device access + host write | New superseding plan when eligible |
| `rescue list` | read-only | Native Loader observation IDs |
| `rescue inspect` | read-only | Parsed device table plus evidence digest |
| `rescue read` | device read + host write | Output file hash and typed read receipt |
| `rescue plan` | read-only device access + host write | Sealed native rescue plan |
| `rescue apply` | mutating or destructive | Separate RescueReceipt; never FlashReceipt |
| `daemon run` | service lifecycle | Foreground CLI runtime supervisor and mechanics daemon |
| `daemon start` | service lifecycle | Starts the same two-process runtime in the background |
| `daemon stop` | service lifecycle | Refuses while an action is in flight |
| `daemon status` | read-only | Protocol, readiness, authority and active jobs |
| `signing verify` | read-only | Development/release contract verdict |
| `completion` | read-only | Shell completion text |
| `help` | read-only | Human or machine command contract |

`artifact import` and `rescue read` write only caller-selected host storage and
are not device mutations. Their help nevertheless says which file can be created.

## 4. Normal flash workflow

### 4.1 `flash assess`

~~~text
Usage:
  arkforge flash assess --artifact <artifact-id> --profile <id@version>
    --device <observation-id> --intent <full-restore>

Effect:
  Read-only. This command cannot create or execute a plan.

Requires:
  An imported artifact and one exact device observation.

Produces:
  Compatibility, data impact, known effects, unknowns, mechanics maturity,
  authority support, and blockers.

Next:
  If executable is true, run 'arkforge flash plan' with the same inputs.
  If executable is false, follow blockers[].remediation. Do not use rescue as
  an automatic fallback.
~~~

An assessment command exits 0 when the assessment was successfully produced,
even when `executable=false`. The result, not process failure, is the answer.

### 4.2 `flash plan`

~~~text
Usage:
  arkforge flash plan --artifact <artifact-id> --profile <id@version>
    --device <observation-id> --intent <full-restore>

Effect:
  Reads the exact device and writes a sealed plan to the daemon store.
  It does not mutate the device.

Requires:
  A paired CLI authority supervisor, executable mechanics maturity and authority
  support for the exact combination, a compatible artifact, and one exact device
  observation.

Produces:
  plan_id, plan_sha256, device binding, artifact hash, ordered effects,
  required acknowledgements, and an expiry/freshness statement.

Example:
  arkforge --output json flash plan \
    --artifact <artifact-id> \
    --profile org.openharmony.dayu200@1.0.0 \
    --device <observation-id> \
    --intent full-restore

Next:
  arkforge flash apply --plan <plan_id> \
    --expect-plan-sha256 <plan_sha256> \
    --ack data-loss:userdata
~~~

Angle-bracket values are placeholders, not accepted literal IDs. Help JSON marks
them as substitutions, and parse-only tests replace them with typed fixtures before
parsing the example.

`flash plan` never accepts a firmware path. The Agent must import the bytes first,
which gives it a content hash to pin in later decisions.

### 4.3 `flash apply`

~~~text
Usage:
  arkforge flash apply --plan <plan-id> --expect-plan-sha256 <sha256>
    --ack <effect-token> [--ack <effect-token>...] [--detach]

Effect:
  Destructive. Executes only the effects sealed into the referenced plan.

Requires:
  Every token in required_acknowledgements, an exact plan digest, a live paired
  CLI authority supervisor, and fresh device continuity.

Behavior:
  The authority supervisor revalidates every admission snapshot, signs one exact
  single-use permit per step, and performs typed managed control. By default, this
  command watches the job to a terminal state. --detach returns after durable job
  creation while the supervisor continues to drive it. Ctrl-C stops watching; it
  does not cancel or kill an in-flight action. Use 'job cancel' for an explicit
  cancellation request.

Produces:
  A durable job ID, ordered step events, action receipts, verification strength,
  postflight facts, and one terminal classification.

Examples:
  arkforge flash apply \
    --plan plan:01A0180894B7 \
    --expect-plan-sha256 <64-lowercase-hex> \
    --ack data-loss:userdata

  arkforge --output jsonl flash apply \
    --plan plan:01A0180894B7 \
    --expect-plan-sha256 <64-lowercase-hex> \
    --ack data-loss:userdata
~~~

`--detach` changes only whether the command waits. It does not move authority into
the mechanics daemon. If the authority supervisor disappears, the daemon stops at
the existing safe/unknown boundary and never replays work.

### 4.4 Acknowledgement tokens

Tokens are generated from the sealed effect set, not from profile display text.
Examples:

~~~text
data-loss:userdata
overwrite:partition=system
overwrite:partition=userdata
reset:device
recovery:supersedes-job=<job-id>
~~~

For normal full restore, one effect-set-level token such as
`data-loss:userdata` may discharge the published grouped impact; the plan still
lists all nine partition effects. A token not required by the plan is rejected as
`UNEXPECTED_ACKNOWLEDGEMENT` so stale automation cannot appear to approve a new
meaning accidentally.

## 5. CLI authority supervisor

### 5.1 Pairing and ownership

`daemon start` creates a dedicated CLI runtime containing two processes: an
authority supervisor and `arkforged`. The supervisor generates the pairing secret
in memory and passes it through an inherited anonymous pipe; the secret is never
an argv, environment variable or plaintext file. Controller handshake exposes
the paired authority namespace and epoch, but never the secret. Later `arkforge`
commands authenticate to the supervisor's owner-only socket and never receive the
pairing secret.

An ArkDeck-owned runtime returns:

~~~json
{
  "code": "AUTHORITY_ALREADY_PAIRED",
  "message": "This runtime is paired with authority 'arkdeck.runtime'.",
  "remediation": "Choose a separate --runtime-dir for direct CLI flash.",
  "retryable": false,
  "next_commands": [
    "arkforge --runtime-dir <new-dir> daemon start"
  ]
}
~~~

The CLI must not offer a takeover flag.

If the supervisor restarts, it rotates the pairing epoch and must coordinate a
safe `arkforged` restart because the old daemon still holds the old in-memory
secret. It first reads public durable job status. It never kills an in-flight
action to regain controller access; an interrupted dispatch follows the existing
unknown-outcome classification. Old-epoch permits that were stored but never
consumed remain audit evidence and can never be first-consumed after restart.

### 5.2 Local target binding

The CLI authority supervisor stores a binding containing the selected observation, profile,
stable identity evidence, mode lineage and revision. `device list` never chooses a
default. `flash plan` requires `--device`; zero, stale or multiple matching devices
are typed refusals.

If the target begins in HDC normal mode, the CLI typed HDC port must establish:

~~~text
command accepted
AND exact bound target disconnected
AND one allowed Loader observation rebound
~~~

If the target begins in Loader, the plan omits the normal-only enter step. Full
normal-flash success still requires reset, exact lineage re-adoption and build
postflight through HDC. Without HDC postflight, the job cannot claim normal success;
the operator may instead choose the separately reported rescue semantics.

### 5.3 Permit loop

For every `STEP_ADMISSION_REQUESTED` event the authority supervisor:

1. recomputes device facts from its binding instead of echoing the daemon digest;
2. checks plan, action, effect, controller session, attempt and freshness;
3. constructs one single-use permit;
4. durably stores the exact canonical CBOR and integrity tag;
5. submits those exact bytes;
6. records the returned receipt before considering the step complete.

Within the same pairing epoch, a transport retry replays the stored bytes exactly.
Across a supervisor restart, the epoch changes: a pending safe step receives a new
admission/attempt and a new permit, while an action with an uncertain dispatch is
never retried.

The authority-side signing module and supervisor live outside `arkforged`; the
existing architecture guard that prevents daemon permit minting remains mandatory.

### 5.4 Two execution gates

The existing `MaturityKey` remains the seven-axis mechanics claim. Authority
behavior is released through a separate key rather than being smuggled into
`evidence_set_digest`:

~~~text
AuthoritySupportKey
  authority namespace
  authority implementation version + digest
  managed-control mapping digest
  permit codec/version digest
  mechanics MaturityKey digest
  host platform
~~~

Executable materialization requires both:

~~~text
mechanics maturity permits execution
AND authority support permits execution
~~~

The plan seals both states and both key digests. An ArkDeck authority support
record cannot authorize `arkforge.cli`, and a CLI supervisor rebuild cannot inherit
the prior build's support. HardwareCampaign remains the named way to obtain the
first evidence; campaign receipts do not become production evidence automatically.

## 6. Explicit native RockUSB rescue

### 6.1 Native binding

Every rescue invocation uses ArkForge's compiled `NativeRockUsbPort`. It enumerates
only the shipped profile's exact Loader USB interface and assigns each observation
a digest-derived ID. Read and apply commands require that ID and reopen the exact
descriptor; no command selects the first matching device.

There is no rescue-backend option, device executable, PATH lookup, device-tool
subprocess or forwarded argv. Ordinary host services used by shared storage code,
such as free-space measurement, are not device backends. The rescue plan seals the
running ArkForge build digest as its native toolchain identity, so a different build
must create a new plan.

### 6.2 Read-only rescue commands

~~~text
arkforge rescue list \
  [--output <human|json>]

arkforge rescue inspect \
  --device <rescue-observation-id>

arkforge rescue read \
  --device <rescue-observation-id> \
  --start-sector <u64> --sector-count <u64> --out <file>
~~~

`rescue read` caps one request at 512 MiB, refuses to overwrite an existing file,
hashes and syncs the output, and reports the native read evidence. It reports bytes,
not an interpretation that filler bytes prove erased media.

### 6.3 `rescue plan`

~~~text
Usage:
  arkforge rescue plan --runtime-dir <dir> --device <rescue-observation-id>
    --operation <write-partition|reset-device> [operation options]

Write-partition options:
  --partition <name> --image <file> --expect-image-sha256 <sha256>

Effect:
  Reads device/native-build/image facts and writes a sealed RescuePlan. It does not
  mutate the device.

Produces:
  rescue_plan_id, rescue_plan_sha256, exact native build/device/image bindings,
  effect classification, evidence limits, and acknowledgement tokens.
~~~

`write-partition` is by parsed partition name only. The plan requires the device
partition table to contain that exact name and records its observed extent. It
does not expose raw LBA write.

### 6.4 `rescue apply`

~~~text
Usage:
  arkforge rescue apply --plan <rescue-plan-id>
    --expect-plan-sha256 <sha256>
    --ack rescue:native-rockusb
    --ack <operation-token>

Effect:
  Mutating or destructive, exactly as classified by the RescuePlan.

Produces:
  RescueReceipt with native build binding, exact device observation, typed semantic
  action, protocol evidence digest, timing, disposition, and evidence limits.

Examples:
  arkforge rescue apply --plan rescue-plan:<id> \
    --expect-plan-sha256 <sha256> \
    --ack rescue:native-rockusb \
    --ack overwrite:partition=uboot

  arkforge rescue apply --plan rescue-plan:<id> \
    --expect-plan-sha256 <sha256> \
    --ack rescue:native-rockusb \
    --ack reset:device
~~~

Rescue receipts use a different type URL/schema and store prefix. Code that reads
normal `ActionReceiptSummary` cannot decode them. A successful native protocol
receipt is still not normal-flash verification because rescue has no normal
authority, full-plan verification or HDC postflight.

## 7. Durable job commands

~~~text
arkforge job list
arkforge job show --job <job-id>
arkforge job watch --job <job-id> [--after-sequence <u64>]
arkforge job cancel --job <job-id> --expect-sequence <u64>
arkforge job reconcile --job <job-id>
arkforge job recovery guide --job <job-id>
arkforge job recovery plan --job <job-id> --artifact <artifact-id> \
  --profile <id@version> --device <observation-id>
~~~

`cancel` uses the last observed journal sequence as optimistic concurrency. It
reports `queued-at-safe-boundary`, `cancelled-safe`, `already-terminal` or
`outcome-unknown`; it never prints only `cancelled`.

Recovery materializes a new plan with a new authority binding, reservation,
intent and epoch. It never edits or resumes the original outcome-unknown plan.

## 8. Daemon runtime

`daemon run` and `daemon start` construct the same runtime; only foreground versus
background supervision differs:

~~~text
arkforge daemon run|start
  [--profile-file <file>]...
  [--hdc <absolute-path> --expect-hdc-sha256 <sha256>]
  [--require-release-signing]
~~~

Shipped profiles are loaded by default. `--profile-file` adds an explicitly named
development/research profile and never overrides a shipped `(id, version)` with
different bytes.

A shipped distribution may bind HDC from its signed tool manifest. A development
run supplies both `--hdc` and `--expect-hdc-sha256`; neither PATH lookup nor an
unhashed executable is allowed. HDC belongs to the authority supervisor and is not
passed into `arkforged`. Without a usable HDC binding the runtime remains available
for inspect/assessment and Loader diagnostics, but normal `flash plan` reports an
authority-support blocker because enter-Loader and postflight cannot be completed.

`daemon status` reports both processes, public/controller protocol versions,
mechanics readiness, authority support, tool bindings, active jobs and typed
blockers. `daemon stop` refuses while an action is in flight. There is no force-stop
flag; an operator can request `job cancel` and stop after a safe terminal state.

## 9. Help manifest for Agents

### 9.1 Schema

`arkforge help --format json` emits `arkforge.command-help/v1` with no hardware
or daemon access. Every leaf includes:

~~~json
{
  "schema": "arkforge.command-help/v1",
  "path": ["flash", "apply"],
  "summary": "Apply one sealed normal-flash plan",
  "effect": "destructive",
  "interactive": false,
  "availability": {
    "platforms": ["macos"],
    "requires_controller": true
  },
  "options": [],
  "constraints": [],
  "outputs": [
    "arkforge.job-event/v1",
    "arkforge.command-result/v1"
  ],
  "exit_codes": [],
  "examples": [],
  "next_commands": []
}
~~~

Each option entry declares type, required/repeatable, enum values, sensitive,
effect relevance and conflicts/requires. Operation-dependent rescue arguments are
represented as JSON Schema-style `oneOf`; an Agent never has to infer them from prose.

### 9.2 Result and error envelopes

Non-streaming JSON output is exactly one document on stdout:

~~~json
{
  "schema": "arkforge.command-result/v1",
  "ok": false,
  "command": ["flash", "apply"],
  "error": {
    "code": "ACKNOWLEDGEMENT_REQUIRED",
    "message": "The plan overwrites userdata.",
    "remediation": "Review the sealed effects and acknowledge the listed token.",
    "retryable": true,
    "required_acknowledgements": ["data-loss:userdata"],
    "next_commands": [
      "arkforge flash apply --plan plan:<id> --expect-plan-sha256 <sha256> --ack data-loss:userdata"
    ]
  }
}
~~~

Streaming JSONL emits an initial metadata record, ordered event records, and one
terminal record. `stream_sequence` is monotonic. A missing terminal record means
the client stream ended, not that the device operation failed or succeeded.

### 9.3 Exit codes

| Exit | Meaning |
|---:|---|
| 0 | Requested observation, plan, operation or terminal success completed |
| 2 | Invalid command or arguments |
| 3 | Preconditions unavailable or policy refused |
| 4 | Required exact acknowledgement was not supplied |
| 5 | Named resource was not found |
| 6 | State conflict, ambiguity or stale sequence |
| 7 | Operation failed before/with a known safe outcome |
| 8 | Outcome unknown; never retry the action automatically |
| 9 | Cancellation queued or client interrupted before a terminal result |
| 10 | Internal failure with no more specific classification |

The stable error `code` is authoritative; exit status is the coarse shell class.

## 10. Unreleased command replacement

There are no compatibility wrappers. Current in-repository entry points are replaced
directly by canonical command handlers, one complete behavior vertical at a time:

| Old form | Canonical form | Notes |
|---|---|---|
| `arkforge-cli --socket S discover` | `arkforge --runtime-dir D device list` | The canonical client derives `D/public.sock`; it never accepts an arbitrary socket capability |
| `... inspect A` | `artifact show --artifact A` | Same imported artifact semantics |
| `... assess A P O` | `flash assess --artifact A --profile P --device O --intent full-restore` | Old implicit intent becomes explicit in canonical help |
| `... jobs` | `job list` | No semantic change |
| `... job J` | `job show --job J` | No semantic change |
| `... recovery-guide J` | `job recovery guide --job J` | No semantic change |
| `arkforge-inspect --archive F --store D --profile P` | `artifact import --file F`, then `artifact inspect --artifact A --profile P` | Two explicit resource operations replace the old composed path |
| `arkforge-signing F` | `signing verify --file F --mode development` | Mode is explicit in canonical help |
| `arkforge-signing F --release` | `signing verify --file F --mode release` | No semantic change |

The new CLI also exposes currently unwrapped IPC capabilities: device probe,
watch/cancel/reconcile, superseding recovery planning, controller materialization
and start execution.

Replacement order is intentionally dependency-shaped:

1. `signing verify` proves the shared command/help/error contract without a daemon;
2. public-socket device/artifact/job queries establish the runtime client;
3. artifact import/inspect establishes host-write resource creation;
4. only then may CLI authority and normal `flash plan/apply` build on those surfaces.

Each vertical removes its old binary and updates every repository caller in the same
commit. A canonical command is never shipped beside a compatibility alias.

## 11. Help and parser verification

The command definition is one typed tree used to generate parsing, human help,
JSON help and shell completions. Separate handwritten copies are forbidden.

Tests must prove:

- every leaf has summary, effect, prerequisites, output schema, exit codes,
  example and next command;
- all examples parse in a no-I/O parse-only harness;
- every destructive leaf requires plan ID, expected digest and exact tokens;
- no command accepts a rescue backend/tool/path/argv flag;
- no rescue command accepts arbitrary trailing argv;
- JSON/JSONL contain no ANSI, progress text or secret-shaped fields;
- source and binary scans find no runtime `rkdeveloptool` dependency or spawn path;
- human help snapshots and `arkforge.command-help/v1` fixtures are stable;
- removed entry points are absent from the packaged binary list.
