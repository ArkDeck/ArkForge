"""Host-only recovery orchestration tests. No physical transport is opened."""
import copy
import contextlib
import importlib.util
import io
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("boot_loop", Path(__file__).with_name("restore-boot-loop.py"))
boot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(boot)


def observation(mode="rockusb-loader"):
    return {"observation_id": "fixture-observation", "mode": mode,
            "identity_strength": "serialAndTopology", "malformed_descriptor": False,
            "serial_sha256": "1" * 64, "topology_sha256": "2" * 64,
            "descriptor_sha256": ("3" if mode == "rockusb-loader" else "4") * 64}


def source(state="rebindWait"):
    completed = 23 if state == "succeeded" else 22
    events = []
    for mode in boot.MODES:
        admission = observation(mode)
        admission.update(job_id="source", plan_id="source-plan", observed_mode=mode)
        events.append({"job_id": "source", "admission": admission})
    for number in range(1, completed + 1):
        events.append({"job_id": "source", "receipt": {"job_id": "source", "plan_id": "source-plan",
                       "step_id": f"STEP-{number:03}", "disposition": "semanticSuccess"}})
    for number, event in enumerate(events, 1):
        event["sequence"] = number
    return {"job": {"job_id": "source", "plan_id": "source-plan", "state": state,
            "completed_steps": completed, "total_steps": 23, "last_sequence": len(events)}, "events": events}


def plan(mode):
    kinds = ["probeDevice", "validateLayout", "writeTarget", "verifyTarget", "reboot", "postflightProbe"]
    if mode == "hdc-normal":
        kinds.insert(0, "ensureMode")
    steps = []
    for number, kind in enumerate(kinds, 1):
        partition = kind in ("writeTarget", "verifyTarget")
        steps.append({"step_id": f"STEP-{number:03}", "kind": kind,
                      "semantic_target": "partition:ramdisk" if partition else "device",
                      "content_sha256": boot.RESTORATION if partition else ""})
    return {"assessment": {"executable": True}, "resolved": {
            "artifact": {"artifact_id": "a" * 64}, "device": {"observation_id": "fixture-observation"},
            "profile": {"reference": boot.PROFILE}}, "plan": {
            "plan_id": "restoration-plan", "plan_sha256": "b" * 64, "execution_purpose": "primaryFlash",
            "execution_context": {"hardware_campaign": "fixture-campaign"},
            "persistent_effects": [copy.deepcopy(boot.EFFECT)], "required_acknowledgements": [boot.ACK],
            "ordered_steps": steps}}


class FakeRuntime:
    def __init__(self, mode="rockusb-loader"):
        self.source = source()
        self.mode = mode
        self.calls = []
        self.saved = {}
        self.devices = [{"observations": [observation(mode)]}]
        self.current_plan = plan(mode)
        self.apply_error = None
        self.plan_errors = []
        self.verification = {"verified": 1, "skipped": 0, "failed": 0, "outcome": "verified",
                             "record_id": "fixture-verification", "targets": [{
                             "verification_strength": "fullHash", "observed_sha256": boot.RESTORATION}]}

    def save(self, name, value):
        if name in self.saved:
            raise FileExistsError(name)
        self.saved[name] = value

    def call(self, name, argv, timeout=660):
        self.calls.append((name, argv))
        if name == "watch":
            if "restoration-job" in argv:
                return {"job": {"terminal": True, "state": "succeeded"}}
            return copy.deepcopy(self.source)
        if name.startswith("devices"):
            return copy.deepcopy(self.devices.pop(0) if len(self.devices) > 1 else self.devices[0])
        if name == "cancel":
            self.source = source("cancelledSafe")
            return {"disposition": "cancelled-safe"}
        if name == "plan":
            assert argv[argv.index("--source-job") + 1] == "source"
            if self.plan_errors:
                raise self.plan_errors.pop(0)
            return copy.deepcopy(self.current_plan)
        if name == "apply":
            assert "apply-intent" in self.saved
            if self.apply_error:
                raise self.apply_error
            return {"job_id": "restoration-job", "detached": True, "authority_continues": True}
        if name == "verify":
            return copy.deepcopy(self.verification)
        raise AssertionError((name, argv))


def args():
    return SimpleNamespace(source_job="source", artifact="a" * 64, hardware_campaign="fixture-campaign",
                           wait_seconds=2, job_timeout_seconds=2)


def run(runtime):
    ticks = iter(range(100))
    with contextlib.redirect_stdout(io.StringIO()):
        boot.restore(runtime, args(), now=lambda: next(ticks), sleep=lambda _: None)


class BootLoopTests(unittest.TestCase):
    def test_delayed_loader_cancels_safe_boundary_and_applies_once(self):
        runtime = FakeRuntime()
        runtime.devices.insert(0, {"observations": []})
        run(runtime)
        self.assertEqual([name for name, _ in runtime.calls].count("apply"), 1)
        self.assertEqual([name for name, _ in runtime.calls].count("cancel"), 1)
        self.assertTrue(runtime.saved["summary"]["ramdiskRestored"])
        self.assertFalse(runtime.saved["summary"]["userdataVerified"])

    def test_normal_mode_uses_seven_step_plan(self):
        runtime = FakeRuntime("hdc-normal")
        run(runtime)
        self.assertTrue(runtime.saved["summary"]["ramdiskRestored"])

    def test_already_completed_source_does_not_cancel(self):
        runtime = FakeRuntime()
        runtime.source = source("succeeded")
        run(runtime)
        self.assertNotIn("cancel", [name for name, _ in runtime.calls])

    def test_no_usb_times_out_without_cancel_plan_or_apply(self):
        runtime = FakeRuntime()
        runtime.devices = [{"observations": []}]
        with self.assertRaisesRegex(boot.Refused, "No usable USB"):
            run(runtime)
        self.assertTrue(all(name in ("watch", "devices") for name, _ in runtime.calls))

    def test_wrong_device_weak_identity_and_ambiguity_never_cancel_or_write(self):
        for mutation in (lambda d: d.update(serial_sha256="e" * 64),
                         lambda d: d.update(topology_sha256="e" * 64),
                         lambda d: d.update(descriptor_sha256="e" * 64),
                         lambda d: d.update(identity_strength="topologyOnly"),
                         lambda d: d.update(malformed_descriptor=True)):
            runtime = FakeRuntime()
            mutation(runtime.devices[0]["observations"][0])
            with self.assertRaises(boot.Refused):
                run(runtime)
            self.assertEqual([name for name, _ in runtime.calls], ["watch", "devices"])
        runtime = FakeRuntime()
        runtime.devices[0]["observations"] *= 2
        with self.assertRaisesRegex(boot.Refused, "Multiple"):
            run(runtime)

    def test_source_unknown_partial_missing_or_newer_job_prevents_device_access(self):
        for mutation in (lambda d: d["job"].update(state="outcomeUnknown"),
                         lambda d: d["job"].update(completed_steps=12),
                         lambda d: d["job"].update(job_id="newer-job"),
                         lambda d: d["events"].pop(),
                         lambda d: d["events"][-1]["receipt"].update(disposition="confirmedPartialEffect")):
            runtime = FakeRuntime()
            mutation(runtime.source)
            with self.assertRaises(boot.Refused):
                run(runtime)
            self.assertEqual([name for name, _ in runtime.calls], ["watch"])

    def test_disappearance_during_planning_never_applies_stale_plan(self):
        runtime = FakeRuntime()
        runtime.devices.append({"observations": []})
        with self.assertRaisesRegex(boot.Refused, "No usable USB"):
            run(runtime)
        self.assertNotIn("apply", [name for name, _ in runtime.calls])

    def test_read_only_target_disappearance_resumes_listening_before_one_apply(self):
        runtime = FakeRuntime()
        runtime.plan_errors = [boot.CommandFailed("target disappeared", "OBSERVATION_NOT_FOUND")]
        run(runtime)
        self.assertEqual([name for name, _ in runtime.calls].count("plan"), 2)
        self.assertEqual([name for name, _ in runtime.calls].count("apply"), 1)

    def test_other_planning_refusal_stops_without_write(self):
        runtime = FakeRuntime()
        runtime.plan_errors = [boot.CommandFailed("lineage differs", "TARGET_LINEAGE_CONFLICT")]
        with self.assertRaisesRegex(boot.Refused, "lineage differs"):
            run(runtime)
        self.assertNotIn("apply", [name for name, _ in runtime.calls])
        self.assertNotIn("cancel", [name for name, _ in runtime.calls])

    def test_restart_keeps_sparse_receipts_without_live_admissions(self):
        runtime = FakeRuntime()
        runtime.source = source("cancelledSafe")
        runtime.source["events"] = [e for e in runtime.source["events"] if "receipt" in e]
        for number, event in enumerate(runtime.source["events"], 1):
            event["sequence"] = number * 5
        runtime.source["job"]["last_sequence"] = runtime.source["events"][-1]["sequence"]
        run(runtime)
        self.assertEqual([name for name, _ in runtime.calls].count("apply"), 1)
        self.assertNotIn("cancel", [name for name, _ in runtime.calls])

    def test_sparse_snapshot_does_not_hide_missing_or_duplicate_receipts(self):
        for mutation in (lambda events: events.pop(4),
                         lambda events: events[-1]["receipt"].update(step_id="STEP-001"),
                         lambda events: events[-1].update(sequence=events[0]["sequence"])):
            runtime = FakeRuntime()
            mutation(runtime.source["events"])
            with self.assertRaises(boot.Refused):
                run(runtime)
            self.assertEqual([name for name, _ in runtime.calls], ["watch"])

    def test_durable_identity_refusal_after_restart_never_cancels_or_applies(self):
        runtime = FakeRuntime()
        runtime.source["events"] = [e for e in runtime.source["events"] if "receipt" in e]
        runtime.plan_errors = [boot.CommandFailed("durable source differs", "TARGET_LINEAGE_CONFLICT")]
        with self.assertRaisesRegex(boot.Refused, "durable source differs"):
            run(runtime)
        self.assertNotIn("cancel", [name for name, _ in runtime.calls])
        self.assertNotIn("apply", [name for name, _ in runtime.calls])

    def test_plan_scope_digest_campaign_and_step_tampering_refuse_before_apply(self):
        for mutation in (lambda d: d["plan"]["persistent_effects"].append(copy.deepcopy(boot.EFFECT)),
                         lambda d: d["plan"]["persistent_effects"][0].update(content_sha256="e" * 64),
                         lambda d: d["plan"]["persistent_effects"][0].update(range_length=2373467),
                         lambda d: d["plan"].update(required_acknowledgements=["data-loss:userdata"]),
                         lambda d: d["plan"]["execution_context"].update(hardware_campaign="other"),
                         lambda d: d["plan"]["ordered_steps"][2].update(semantic_target="partition:userdata")):
            runtime = FakeRuntime()
            mutation(runtime.current_plan)
            with self.assertRaises(boot.Refused):
                run(runtime)
            self.assertNotIn("apply", [name for name, _ in runtime.calls])

    def test_apply_failure_or_lost_reply_never_retries(self):
        for reason in ("outcome unknown", "lost reply", "confirmed no effect"):
            runtime = FakeRuntime()
            runtime.apply_error = boot.Refused(reason)
            with self.assertRaisesRegex(boot.Refused, reason):
                run(runtime)
            self.assertEqual([name for name, _ in runtime.calls].count("apply"), 1)
            self.assertNotIn("summary", runtime.saved)

    def test_partial_or_prefix_verification_is_not_success(self):
        for mutation in (lambda d: d.update(skipped=1),
                         lambda d: d["targets"][0].update(verification_strength="prefixHash"),
                         lambda d: d["targets"][0].update(observed_sha256="e" * 64)):
            runtime = FakeRuntime()
            mutation(runtime.verification)
            with self.assertRaises(boot.Refused):
                run(runtime)
            self.assertNotIn("summary", runtime.saved)

    def test_existing_output_directory_cannot_restart_an_apply(self):
        with tempfile.TemporaryDirectory() as directory:
            options = args()
            options.out_dir = Path(directory)
            with self.assertRaises(FileExistsError):
                boot.Runtime(options)

    def test_subprocess_uses_argv_and_timeout_retains_no_retry_record(self):
        with tempfile.TemporaryDirectory() as directory:
            options = args()
            options.out_dir = Path(directory) / "run"
            options.arkforge = Path(directory) / "arkforge"
            options.runtime_dir = Path(directory) / "runtime"
            runtime = boot.Runtime(options)
            with patch.object(boot.subprocess, "run", side_effect=subprocess.TimeoutExpired("apply", 1)) as process:
                with self.assertRaisesRegex(boot.Refused, "never retry"):
                    runtime.call("apply", ["apply", "--plan", "fixture-plan"], timeout=1)
            self.assertFalse(process.call_args.kwargs["shell"])
            self.assertIsInstance(process.call_args.args[0], list)
            self.assertTrue((options.out_dir / "00001-apply-timeout.json").is_file())


if __name__ == "__main__":
    unittest.main()
