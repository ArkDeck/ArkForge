# Glossary
status: informative

One definition per term, as the spec uses it. Wire spellings are in
`model/vocabularies.yaml`.

| term | definition |
|---|---|
| **Authority** | The party that decides whether an external effect may happen: mints StepPermits, owns target binding and lanes. ArkDeck in production; the standalone supervisor for the CLI. Never the daemon. |
| **Authority binding** | `{authorityNamespace, bindingId, bindingRevision, stableIdentityDigest}` — the authority's reference to the bound target, sealed into the plan. |
| **Admission snapshot** | The facts the daemon read immediately before asking for a permit; digested and re-verified by the authority. |
| **Artifact** | Imported firmware bytes, content-addressed by SHA-256; parsed into a **manifest** (facts, unknowns, confidence). |
| **Assessment (PlanAssessment)** | What materialization returns when an executable plan is not allowed: candidates, known effects, unknowns paired with evidence requirements, availability. Has no plan id or digest. |
| **Canonical CBOR** | RFC 8949 §4.2.1 deterministic encoding restricted to the value subset of AF-DIG-003; the digest model. |
| **CAS** | Content-addressed store for artifacts. |
| **Checkpoint** | The durable record (`stepCheckpointed`) that a step's receipt was verified and the job may advance. |
| **Conformance case** | A directory under `conformance/v1/<suite>/AF-CONF-<SUITE>-<NNN>/` with `case.json` and exact input/expected bytes. |
| **Continuity** | The primary freshness fact: same transport session, no detach, unchanged device/provider/toolchain/artifact facts. |
| **Crash disposition** | The one row of `state-machines/crash-disposition.yaml` that applies to a replayed journal. |
| **Domain (digest)** | The NUL-terminated ASCII prefix hashed before a payload so equal bytes in different roles hash differently. |
| **Durable / buffered** | Journal fsync policies: durable returns only after stable storage; buffered is ordered behind the next durable record. |
| **Effect set** | Persistent effects, transient effects and the four data-impact axes a plan declares. |
| **Hardware campaign** | A named, operator-opened acceptance run that permits an executable plan for an unverified combination without publishing support. |
| **Journal** | The per-job append-only, hash-chained record file the daemon writes; the sole source of truth after a crash. |
| **Managed device control** | The four semantic HDC actions the authority performs on the daemon's request. |
| **Maturity** | The published state of an exact combination (provider, profile, artifact format, toolchain, host, driver facts, evidence): `productionVerified`, `hardwareCampaign`, `hardwareGated`, `planOnly`, `researchOnly`, `unavailable`. |
| **Observation** | A typed, digested view of a device at one instant, with an identity strength. |
| **outcomeUnknown** | A job state (and receipt disposition) meaning the device may or may not have been touched; never replayed, only reconciled. |
| **Pairing secret / epoch** | The in-memory HMAC key shared by authority and daemon at daemon start, and its rotation counter. |
| **Plan (FlashPlanEnvelope)** | The sealed, immutable, digested public description of one execution. |
| **Port** | A contract at an OS/hardware/process boundary (`ports/`). |
| **Private action** | A provider-shaped action inside the private execution plan; only its digest crosses to the authority. |
| **Profile (DeviceProfile)** | Published data about a board: identity, modes, transitions, storage, read domain, allowed/protected targets, data impact, recovery coverage. |
| **Projection** | The binding between public steps and private actions, with its two digests. |
| **Provider** | The component that lowers a plan to private actions for one protocol family (Rockchip, Unisoc). |
| **Read domain** | The part of the medium the device's read face can actually return; measured at runtime. |
| **Rebind** | Re-identifying the same device after a mode transition and re-enumeration. |
| **Receipt** | The semantic outcome of one action: disposition, verification outcome/strength, facts, evidence digest. |
| **Reconcile** | A read-only attempt to learn what an unresolved action did. |
| **Rescue** | A separate typed workflow and digest domain for recovering a device outside a normal flash. |
| **StepPermit** | A single-use, HMAC-tagged authorization for exactly one private action of one plan step. |
| **Superseding recovery** | A distinct new plan that completely overwrites the uncertain effects of an unknown outcome. |
| **Toolchain** | The execution backend identity (`nativeProtocol`, `replay`, retired `fixedTool`) with its digest. |
| **Transcript** | A recorded interaction that the replay transport plays back. |
| **Typed skip** | A verification outcome meaning the read domain could not cover the range; never a failure and never a strength. |
