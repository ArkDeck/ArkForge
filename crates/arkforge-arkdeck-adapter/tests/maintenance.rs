//! Fake authority/transport tests: never hardware acceptance.
use arkforge_arkdeck_adapter::maintenance::*;
use arkforge_ipc::messages::{
    ActionReceiptSummary, Effect, ExecutablePlan, JobSummary, PublicStep,
};
use std::time::Instant;

fn source(restored: bool) -> Source {
    let names = if restored {
        vec!["ramdisk"]
    } else {
        vec![
            "uboot",
            "resource",
            "boot_linux",
            "ramdisk",
            "system",
            "vendor",
            "updater",
            "chip_ckm",
            "userdata",
        ]
    };
    let effects = names
        .into_iter()
        .enumerate()
        .map(|(i, target)| Effect {
            kind: "writePartition".into(),
            target: target.into(),
            range_start: if restored {
                RAMDISK_START
            } else {
                (i as u64 + 1) * 4096
            },
            range_length: if restored { RAMDISK_BYTES } else { 4096 },
            content_sha256: if restored {
                RESTORATION_SHA256.into()
            } else {
                "aa".repeat(32)
            },
        })
        .collect::<Vec<_>>();
    let mut steps = Vec::new();
    for kind in ["probeDevice", "validateLayout"] {
        steps.push(PublicStep {
            step_id: format!("STEP-{}", steps.len()),
            kind: kind.into(),
            ..Default::default()
        });
    }
    for effect in &effects {
        for kind in ["writeTarget", "verifyTarget"] {
            steps.push(PublicStep {
                step_id: format!("STEP-{}", steps.len()),
                kind: kind.into(),
                semantic_target: format!("partition:{}", effect.target),
                content_sha256: effect.content_sha256.clone(),
                ..Default::default()
            });
        }
    }
    for kind in ["reboot", "postflightProbe"] {
        steps.push(PublicStep {
            step_id: format!("STEP-{}", steps.len()),
            kind: kind.into(),
            ..Default::default()
        });
    }
    let job_id = if restored {
        "JOB-RESTORED"
    } else {
        "JOB-SOURCE"
    };
    let plan_id = if restored {
        "PLAN-RESTORED"
    } else {
        "PLAN-SOURCE"
    };
    let receipts = steps
        .iter()
        .map(|step| ActionReceiptSummary {
            job_id: job_id.into(),
            plan_id: plan_id.into(),
            step_id: step.step_id.clone(),
            attempt_id: "ATTEMPT-TEST".into(),
            permit_id: "PERMIT-TEST".into(),
            disposition: "semanticSuccess".into(),
            evidence_sha256: vec![1; 32],
            ..Default::default()
        })
        .collect();
    Source {
        authority_namespace: "arkdeck".into(),
        profile: "org.openharmony.dayu200@1.0.0".into(),
        campaign: "campaign".into(),
        job: JobSummary {
            job_id: job_id.into(),
            plan_id: plan_id.into(),
            plan_sha256: vec![0xaa; 32],
            state: "succeeded".into(),
            terminal: true,
            total_steps: steps.len() as u64,
            completed_steps: steps.len() as u64,
            ..Default::default()
        },
        plan: ExecutablePlan {
            plan_id: plan_id.into(),
            plan_sha256: "aa".repeat(32),
            public_steps: steps,
            persistent_effects: effects,
            execution_purpose: "primaryFlash".into(),
            authority_support_campaign: "campaign".into(),
            mechanics_maturity_campaign: "campaign".into(),
            ..Default::default()
        },
        receipts,
        identity: Identity {
            serial_sha256: "bb".repeat(32),
            topology_sha256: "cc".repeat(32),
            descriptor_sha256: "dd".repeat(32),
        },
        latest_job: job_id.into(),
        active_jobs: 0,
    }
}
struct Port {
    source: Source,
    restored: Source,
    calls: Vec<&'static str>,
    read: Option<ReadOutcome>,
    identity_calls: usize,
    drift_at: Option<usize>,
    fail_intent: bool,
    fail_result: bool,
    fail_execute: bool,
    reserved: bool,
}
impl Default for Port {
    fn default() -> Self {
        Self {
            source: source(false),
            restored: source(true),
            calls: vec![],
            read: None,
            identity_calls: 0,
            drift_at: None,
            fail_intent: false,
            fail_result: false,
            fail_execute: false,
            reserved: false,
        }
    }
}
impl VerificationPort for Port {
    fn load_source(&mut self, job: &str) -> Result<Source, Error> {
        self.calls.push("load");
        Ok(if job == "JOB-SOURCE" {
            self.source.clone()
        } else {
            self.restored.clone()
        })
    }
    fn fresh_identity(&mut self, source: &Source, _: Instant) -> Result<Identity, Error> {
        self.calls.push("identity");
        self.identity_calls += 1;
        if self.drift_at == Some(self.identity_calls) {
            return Err(Error::IdentityChanged);
        }
        Ok(source.identity.clone())
    }
    fn verification_intent(&mut self, _: &Source, _: u64) -> Result<(), Error> {
        self.calls.push("intent");
        if self.fail_intent {
            Err(Error::PersistenceFailed)
        } else {
            Ok(())
        }
    }
    fn read_partition(
        &mut self,
        _: &Source,
        effect: &Effect,
        _: Instant,
    ) -> Result<ReadOutcome, Error> {
        self.calls.push("read");
        Ok(self
            .read
            .clone()
            .unwrap_or_else(|| ReadOutcome::FullHash(effect.content_sha256.clone())))
    }
    fn verification_result(&mut self, _: &VerificationResult) -> Result<(), Error> {
        self.calls.push("result");
        if self.fail_result {
            Err(Error::PersistenceFailed)
        } else {
            Ok(())
        }
    }
}
impl RestorationPort for Port {
    type Reservation = ();
    fn restoration_plan(
        &mut self,
        _: &Source,
        _: &str,
        _: Instant,
    ) -> Result<ExecutablePlan, Error> {
        self.calls.push("plan");
        Ok(self.restored.plan.clone())
    }
    fn reserve_restoration(
        &mut self,
        _: &Source,
        _: &ExecutablePlan,
        _: Instant,
    ) -> Result<(), Error> {
        self.calls.push("reserve");
        if self.reserved {
            return Err(Error::AuthorityRefused("already used".into()));
        }
        self.reserved = true;
        Ok(())
    }
    fn execute_restoration(&mut self, _: (), _: Instant) -> Result<String, Error> {
        self.calls.push("execute");
        if self.fail_execute {
            Err(Error::AuthorityRefused("unknown outcome".into()))
        } else {
            Ok("JOB-RESTORED".into())
        }
    }
    fn restoration_result(&mut self, _: &str, _: &RestorationResult) -> Result<(), Error> {
        self.calls.push("restoration-result");
        if self.fail_result {
            Err(Error::PersistenceFailed)
        } else {
            Ok(())
        }
    }
}
#[test]
fn verify_preserves_three_outcomes_and_persists_before_reads() {
    for (read, code) in [
        (None, 0),
        (Some(ReadOutcome::MountedWritable), 9),
        (Some(ReadOutcome::FullHash("ee".repeat(32))), 7),
        (Some(ReadOutcome::Failed("short-read".into())), 7),
    ] {
        let mut port = Port {
            read,
            ..Default::default()
        };
        let result = verify(&mut port, "JOB-SOURCE", 10_000).unwrap();
        assert_eq!(result.exit_code(), code);
        assert_eq!(result.targets.len(), 9);
        assert!(
            port.calls.iter().position(|c| *c == "intent")
                < port.calls.iter().position(|c| *c == "read")
        );
        assert_eq!(port.calls.last(), Some(&"result"));
        assert!(!port.calls.contains(&"execute"));
    }
}
#[test]
fn source_and_identity_fail_closed_without_reads() {
    for mutate in [
        (|s: &mut Source| s.job.state = "outcomeUnknown".into()) as fn(&mut Source),
        |s| s.latest_job = "JOB-NEWER".into(),
        |s| s.receipts.pop().map(drop).unwrap(),
        |s| s.receipts[0].disposition = "outcomeUnknown".into(),
        |s| s.plan.persistent_effects[0].range_length = 0,
        |s| s.campaign = "different".into(),
        |s| s.authority_namespace = "arkforge.cli".into(),
        |s| s.plan.plan_sha256 = "ff".repeat(32),
        |s| s.active_jobs = 1,
    ] {
        let mut port = Port::default();
        mutate(&mut port.source);
        assert!(verify(&mut port, "JOB-SOURCE", 10_000).is_err());
        assert!(!port.calls.contains(&"intent"));
        assert!(!port.calls.contains(&"read"));
    }
    let mut port = Port {
        drift_at: Some(1),
        ..Default::default()
    };
    assert_eq!(
        verify(&mut port, "JOB-SOURCE", 10_000),
        Err(Error::IdentityChanged)
    );
    assert!(!port.calls.contains(&"read"));
}
#[test]
fn identity_lost_after_read_invalidates_prior_success_and_stops_reads() {
    let mut port = Port {
        drift_at: Some(5),
        ..Default::default()
    };
    let result = verify(&mut port, "JOB-SOURCE", 10_000).unwrap();
    assert_eq!(
        (result.exit_code(), result.verified(), result.failed()),
        (7, 0, 9)
    );
    assert_eq!(port.calls.iter().filter(|c| **c == "read").count(), 2);
}
#[test]
fn persistence_errors_cannot_be_success() {
    let mut port = Port {
        fail_intent: true,
        ..Default::default()
    };
    assert_eq!(
        verify(&mut port, "JOB-SOURCE", 10_000),
        Err(Error::PersistenceFailed)
    );
    assert!(!port.calls.contains(&"read"));
    let mut port = Port {
        fail_result: true,
        ..Default::default()
    };
    assert_eq!(
        verify(&mut port, "JOB-SOURCE", 10_000),
        Err(Error::PersistenceFailed)
    );
}
#[test]
fn restoration_success_requires_a_separate_full_hash_and_one_consumption() {
    let mut port = Port::default();
    let result = restore(&mut port, "JOB-SOURCE", &"aa".repeat(32), 10_000).unwrap();
    assert_eq!(result.exit_code(), 0);
    assert_eq!(port.calls.iter().filter(|c| **c == "execute").count(), 1);
    assert!(
        port.calls.iter().position(|c| *c == "reserve")
            < port.calls.iter().position(|c| *c == "execute")
    );
    assert!(
        port.calls.iter().position(|c| *c == "execute")
            < port.calls.iter().position(|c| *c == "read")
    );
    assert_eq!(
        restore(&mut port, "JOB-SOURCE", &"aa".repeat(32), 10_000)
            .unwrap()
            .exit_code(),
        1
    );
    assert_eq!(port.calls.iter().filter(|c| **c == "execute").count(), 1);
}
#[test]
fn restoration_never_retries_unknown_or_claims_a_skip_as_success() {
    for (fail_execute, read) in [(true, None), (false, Some(ReadOutcome::MountedWritable))] {
        let mut port = Port {
            fail_execute,
            read,
            ..Default::default()
        };
        assert_eq!(
            restore(&mut port, "JOB-SOURCE", &"aa".repeat(32), 10_000)
                .unwrap()
                .exit_code(),
            1
        );
        assert_eq!(port.calls.iter().filter(|c| **c == "execute").count(), 1);
        assert_eq!(port.calls.last(), Some(&"restoration-result"));
    }
}
#[test]
fn invalid_input_has_no_record_and_wrong_restoration_has_no_dispatch() {
    let mut port = Port::default();
    assert_eq!(
        restore(&mut port, "JOB-SOURCE", "not-a-digest", 10_000),
        Err(Error::InvalidInput)
    );
    assert!(port.calls.is_empty());
    port.restored.plan.persistent_effects[0].target = "userdata".into();
    assert_eq!(
        restore(&mut port, "JOB-SOURCE", &"aa".repeat(32), 10_000)
            .unwrap()
            .exit_code(),
        1
    );
    assert!(!port.calls.contains(&"reserve"));
    assert!(!port.calls.contains(&"execute"));
}

#[test]
fn unexpected_restoration_steps_and_wrong_target_identity_cannot_dispatch() {
    let mut port = Port::default();
    port.restored.plan.public_steps[0].kind = "eraseTarget".into();
    assert_eq!(
        restore(&mut port, "JOB-SOURCE", &"aa".repeat(32), 10_000)
            .unwrap()
            .exit_code(),
        1
    );
    assert!(!port.calls.contains(&"reserve"));
    let mut port = Port {
        drift_at: Some(2),
        ..Default::default()
    };
    assert_eq!(
        restore(&mut port, "JOB-SOURCE", &"aa".repeat(32), 10_000)
            .unwrap()
            .exit_code(),
        1
    );
    assert!(!port.calls.contains(&"reserve"));
}
