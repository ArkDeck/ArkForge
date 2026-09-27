# Rust local clients

`PublicClient` observes the public endpoint; `ControllerClient` is for the authority's
controller endpoint. Both use the existing local framing and typed IPC messages.

Controller materialization uses `import_artifact(path, expected_sha256)` to stream a
regular file, `artifact_show(artifact_id)` to obtain its manifest, `device_list()` to
obtain observations, then `materialize_plan(&MaterializeInput)`. Import sends the
opened file's size and caller's expected digest, followed by content frames of at
most 4 MiB and an empty terminator. The path stays local. The daemon checks the
digest and returns `arkforge_ipc::messages::ImportArtifactResponse` with `artifact_id`,
`sha256`, `size_bytes` and `deduplicated`.

Both clients offer `connect_with_read_timeout(runtime_dir, Duration)`. The controller's
`MATERIALIZATION_READ_TIMEOUT` is 900 seconds, matching the Swift SDK's archive
materialization wait. It can also be passed to the public constructor for inspection
and `flash_assess`. These are idle bounds on individual reads, **not** write timeouts
or a deadline for the whole exchange. The default `connect` bounds its handshake to
10 seconds and leaves subsequent reads unbounded; callers select a separate bounded
connection when their operation needs one.

An interrupted exchange, malformed envelope or mismatched response closes the
connection without retrying. A refused import also closes it because the daemon may
not have consumed all content frames. Callers must not infer the outcome of an
execution or automatically replay it from a transport error.

Public `flash_assess` keeps its artifact/profile/observation request and fixed
`fullRestore` intent. It sends no controller bindings and rejects an executable plan;
the timeout constructor adds no API or authority to that endpoint.

`tests/controller_materialization.rs` exercises real local channels with scripted
peers. The import payload vectors are paired with the existing Swift codec tests.
These checks do not involve an installed daemon or constitute hardware acceptance.
