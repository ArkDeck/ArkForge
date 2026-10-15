#!/usr/bin/env python3
"""Wait for an exact DAYU200 and restore its original ramdisk through Runtime.

Explicitly authorizes one ramdisk restoration, not a retry of the source job.
The source must have confirmed every step through its final reboot. No raw
transport, authority-store access, rescue fallback, or write retry is used.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

PROFILE = "org.openharmony.dayu200@1.0.0"
ACK = "overwrite:partition=ramdisk"
RESTORATION = "bdd1f4eacbe545f5a553b529980bc487723d37554148058dd551c241abf86de4"
EFFECT = {"kind": "writePartition", "target": "ramdisk", "range_start": 121634816,
          "range_length": 4194304, "content_sha256": RESTORATION}
IDENTITY = ("serial_sha256", "topology_sha256", "descriptor_sha256")
MODES = ("rockusb-loader", "hdc-normal")


class Refused(RuntimeError):
    pass


class CommandFailed(Refused):
    def __init__(self, message, code):
        super().__init__(message)
        self.code = code


def require(condition, message):
    if not condition:
        raise Refused(message)


def valid_digest(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def identity(value):
    require(value.get("identity_strength") == "serialAndTopology"
            and value.get("malformed_descriptor") is False
            and all(valid_digest(value.get(key)) for key in IDENTITY),
            "Strong serial, topology and descriptor identity is required")
    return tuple(value[key] for key in IDENTITY)


def source_identities(document, source_job):
    job, events = document["job"], document["events"]
    require(job["job_id"] == source_job, "Another job is active or newer; inspect it before restoring")
    require(job["state"] in ("rebindWait", "cancelledSafe", "succeeded"),
            "Source outcome is not a confirmed post-reboot boundary; no new dispatch")
    total, completed = job["total_steps"], job["completed_steps"]
    require(total in (22, 23) and completed == total - (job["state"] != "succeeded"),
            "Source is not a completed full-flash reboot with only postflight remaining")
    require([e["sequence"] for e in events] == list(range(1, job["last_sequence"] + 1)),
            "Source journal snapshot is incomplete")
    receipts, modes = {}, {}
    for event in events:
        require(event["job_id"] == source_job, "Mixed source journal")
        receipt = event.get("receipt")
        if receipt:
            require(receipt["job_id"] == source_job and receipt["plan_id"] == job["plan_id"]
                    and receipt["disposition"] == "semanticSuccess"
                    and receipt["step_id"] not in receipts,
                    "Source has missing, duplicate, failed or uncertain action outcomes")
            receipts[receipt["step_id"]] = receipt
        admission = event.get("admission")
        if admission:
            require(admission["job_id"] == source_job and admission["plan_id"] == job["plan_id"],
                    "Mixed source admissions")
            mode = admission["observed_mode"]
            require(mode in MODES, "Unsupported source mode")
            key = identity(admission)
            require(mode not in modes or modes[mode] == key, "Source identity changed within a mode")
            modes[mode] = key
    require(set(receipts) == {f"STEP-{n:03}" for n in range(1, completed + 1)},
            "Every source action through reboot must have one confirmed success receipt")
    require("rockusb-loader" in modes, "Source has no confirmed Loader identity")
    return modes


def select_device(document, modes):
    observations = document["observations"]
    candidates = [o for o in observations if o.get("mode") in MODES]
    if not candidates:
        return None
    require(len(candidates) == 1, "Multiple supported devices are present; no automatic selection")
    device = candidates[0]
    require(device["mode"] in modes and identity(device) == modes[device["mode"]],
            "Current device differs from the source job's recorded identity")
    return device


def review_plan(document, device, artifact, campaign):
    plan, resolved = document["plan"], document["resolved"]
    require(document["assessment"]["executable"] is True, "Restoration is not executable")
    require(resolved["artifact"]["artifact_id"] == artifact
            and resolved["device"]["observation_id"] == device["observation_id"]
            and resolved["profile"]["reference"] == PROFILE,
            "Restoration resolved different inputs")
    require(plan["execution_context"]["hardware_campaign"] == campaign
            and plan["execution_purpose"] == "primaryFlash"
            and valid_digest(plan["plan_sha256"]), "Restoration context differs")
    require(plan["persistent_effects"] == [EFFECT]
            and plan["required_acknowledgements"] == [ACK],
            "Restoration must overwrite only the pinned complete original ramdisk")
    kinds = ["probeDevice", "validateLayout", "writeTarget", "verifyTarget", "reboot", "postflightProbe"]
    if device["mode"] == "hdc-normal":
        kinds.insert(0, "ensureMode")
    require([s["kind"] for s in plan["ordered_steps"]] == kinds,
            "Unexpected restoration steps")
    for number, step in enumerate(plan["ordered_steps"], 1):
        is_partition = step["kind"] in ("writeTarget", "verifyTarget")
        require(step["step_id"] == f"STEP-{number:03}"
                and step["semantic_target"] == ("partition:ramdisk" if is_partition else "device")
                and step["content_sha256"] == (RESTORATION if is_partition else ""),
                "Restoration step targets differ")
    return plan


class Runtime:
    def __init__(self, args):
        self.args = args
        self.out = args.out_dir
        self.out.mkdir()  # Refuse an old run directory, including an interrupted apply.
        self.index = 0
        self.base = [str(args.arkforge.resolve()), "--runtime-dir", str(args.runtime_dir.resolve()),
                     "--no-auto-start", "--no-input", "--output", "json"]

    def save(self, name, value):
        with (self.out / (name + ".json")).open("x", encoding="utf-8") as stream:
            json.dump(value, stream, indent=2)
            stream.flush()
            os.fsync(stream.fileno())

    def call(self, name, argv, timeout=660):
        self.index += 1
        path = self.out / f"{self.index:05}-{name}.json"
        command = self.base + argv + ["--hardware-campaign", self.args.hardware_campaign]
        try:
            result = subprocess.run(command, capture_output=True, text=True, encoding="utf-8",
                                    timeout=timeout, shell=False)
        except subprocess.TimeoutExpired as error:
            self.save(f"{self.index:05}-{name}-timeout", {"timeoutSeconds": timeout,
                      "automaticReplay": False, "mayContinueInRuntime": name == "apply"})
            raise Refused(f"{name} timed out; inspect Runtime, never retry apply automatically") from error
        with path.open("x", encoding="utf-8") as stream:
            stream.write(result.stdout)
        if result.stderr:
            with path.with_suffix(".stderr.txt").open("x", encoding="utf-8") as stream:
                stream.write(result.stderr)
        value = json.loads(result.stdout)
        if result.returncode != 0:
            raise CommandFailed(f"{name} exited {result.returncode}; retained {path.name}; no write retry",
                                value.get("error", {}).get("code"))
        return value


def watch(runtime, source_job=None, timeout_ms=0):
    argv = ["watch", "--after-sequence", "0", "--timeout-ms", str(timeout_ms)]
    if source_job is not None:
        argv += ["--job", source_job]
    return runtime.call("watch", argv)


def restore(runtime, args, now=time.monotonic, sleep=time.sleep):
    # Default watch resolves the sole active job, or the latest durable job.
    # A prior restoration attempt is therefore not silently skipped on rerun.
    original = watch(runtime)
    modes = source_identities(original, args.source_job)
    deadline = now() + args.wait_seconds
    print("Waiting for the source board in Loader or Normal mode", flush=True)
    while True:
        device = select_device(runtime.call("devices", ["device", "list"]), modes)
        if device is not None:
            # A brief Normal session can let the old job finish its read-only
            # postflight while we are observing it. Allow that transition to settle.
            current = watch(runtime, timeout_ms=1000)
            require(source_identities(current, args.source_job) == modes, "Source identity changed")
            if current["job"]["state"] == "rebindWait":
                cancellation = runtime.call("cancel", ["cancel", "--job", args.source_job,
                    "--expect-sequence", str(current["job"]["last_sequence"])])
                require(cancellation["disposition"] in ("cancelled-safe", "already-terminal"),
                        "Source could not be stopped at its confirmed reboot boundary")
                require(source_identities(watch(runtime), args.source_job) == modes, "Source changed during cancel")
            try:
                result = runtime.call("plan", ["flash", "restore-ramdisk", "--artifact", args.artifact,
                                      "--profile", PROFILE, "--device", device["observation_id"]])
            except CommandFailed as error:
                # These are read-only planning failures. Never catch an apply failure here.
                if error.code not in ("OBSERVATION_NOT_FOUND", "DEVICE_NOT_FOUND"):
                    raise
                result = None
            plan = review_plan(result, device, args.artifact, args.hardware_campaign) if result else None
            # Planning may be slow. Recheck visibility and exact identity; Runtime
            # owns fresh transport-session binding and the final dispatch gate.
            fresh = select_device(runtime.call("devices-before-apply", ["device", "list"]), modes) if plan else None
            if fresh is not None and all(fresh.get(k) == device.get(k) for k in
                    ("observation_id", "mode")):
                require(source_identities(watch(runtime), args.source_job) == modes, "A newer job appeared")
                runtime.save("apply-intent", {"planId": plan["plan_id"], "planSha256": plan["plan_sha256"],
                    "sourceJob": args.source_job, "automaticReplay": False})
                applied = runtime.call("apply", ["apply", "--plan", plan["plan_id"],
                    "--expect-plan-sha256", plan["plan_sha256"], "--ack", ACK, "--detach"])
                require(applied["detached"] is True and applied["authority_continues"] is True,
                        "Apply response is incomplete; inspect Runtime, no retry")
                job_id = applied["job_id"]
                print(f"Restoration submitted once: {job_id}", flush=True)
                break
        if now() >= deadline:
            raise Refused("No usable USB session before the deadline; enter Loader physically. No restoration was submitted")
        sleep(0.1)
    deadline = now() + args.job_timeout_seconds
    while True:
        result = watch(runtime, job_id, 30000)
        job = result["job"]
        if job["terminal"]:
            require(job["state"] == "succeeded", "Restoration did not succeed; inspect its receipts, no retry")
            break
        if now() >= deadline:
            raise Refused(f"Restoration {job_id} is still {job['state']}; Runtime retains control; no retry")
    verification = runtime.call("verify", ["flash", "verify", "--job", job_id, "--budget-ms", "300000"])
    require((verification["verified"], verification["skipped"], verification["failed"], verification["outcome"])
            == (1, 0, 0, "verified"), "Restoration has not fully verified")
    require(len(verification["targets"]) == 1
            and verification["targets"][0]["verification_strength"] == "fullHash"
            and verification["targets"][0]["observed_sha256"] == RESTORATION,
            "Restoration needs a full pinned-ramdisk hash")
    runtime.save("summary", {"schema": "arkforge.boot-loop-restoration/v1", "sourceJob": args.source_job,
        "restorationJob": job_id, "verificationRecord": verification["record_id"], "ramdiskRestored": True,
        "userdataWritten": False, "userdataVerified": False, "automaticReplay": False})
    print("Original ramdisk fully verified; userdata was not rewritten or verified", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arkforge", type=Path, required=True)
    parser.add_argument("--runtime-dir", type=Path, required=True)
    parser.add_argument("--source-job", required=True)
    parser.add_argument("--artifact", required=True, help="Imported pinned restoration bundle ID")
    parser.add_argument("--hardware-campaign", required=True)
    parser.add_argument("--ack", required=True, choices=[ACK])
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument("--wait-seconds", type=int, default=600)
    parser.add_argument("--job-timeout-seconds", type=int, default=600)
    args = parser.parse_args()
    require(valid_digest(args.artifact), "Artifact must be a SHA-256 reference")
    require(1 <= args.wait_seconds <= 3600 and 1 <= args.job_timeout_seconds <= 3600, "Budgets must be 1..3600 seconds")
    runtime = Runtime(args)
    try:
        restore(runtime, args)
    except (Refused, KeyError, TypeError, ValueError) as error:
        runtime.save("stopped", {"reason": str(error), "automaticReplay": False, "ramdiskRestored": False})
        print(str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
