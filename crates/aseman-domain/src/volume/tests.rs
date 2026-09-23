use super::*;

fn volume(name: &str, tier: PortabilityTier, provider: &str) -> Volume {
    Volume {
        name: name.to_owned(),
        tier,
        provider: provider.to_owned(),
        snapshot_format: None,
    }
}

fn placement(worker: &str, runtime: &str, provider: &str) -> Placement {
    Placement {
        worker: worker.to_owned(),
        runtime: runtime.to_owned(),
        provider: provider.to_owned(),
        architecture: "x86_64".to_owned(),
    }
}

#[test]
fn a_stateless_workload_just_moves() {
    let plan = plan_move(
        &[],
        &placement("a", "docker", "nomad"),
        &placement("b", "docker", "nomad"),
    )
    .expect("a move");
    assert_eq!(plan, MovePlan::Recreate);
}

#[test]
fn a_provider_local_volume_may_not_leave_its_provider() {
    let volumes = [volume("data", PortabilityTier::ProviderLocal, "nomad")];
    // Within the provider: the provider's own snapshot carries it.
    assert_eq!(
        plan_move(
            &volumes,
            &placement("a", "docker", "nomad"),
            &placement("b", "docker", "nomad"),
        )
        .expect("a move within the provider"),
        MovePlan::ProviderSnapshot
    );
    // Across providers: refused, by name, rather than recreated empty.
    assert_eq!(
        plan_move(
            &volumes,
            &placement("a", "docker", "nomad"),
            &placement("b", "docker", "native-legacy"),
        ),
        Err(PortabilityError::ProviderLocalVolume {
            name: "data".to_owned()
        })
    );
}

#[test]
fn a_portable_volume_crossing_runtimes_needs_a_snapshot_format() {
    let mut volume = volume("data", PortabilityTier::PortableOffline, "nomad");
    assert_eq!(
        plan_move(
            std::slice::from_ref(&volume),
            &placement("a", "docker", "nomad"),
            &placement("b", "qemu", "nomad"),
        ),
        Err(PortabilityError::NoSnapshotFormat {
            name: "data".to_owned()
        })
    );
    volume.snapshot_format = Some("qcow2".to_owned());
    assert_eq!(
        plan_move(
            std::slice::from_ref(&volume),
            &placement("a", "docker", "nomad"),
            &placement("b", "qemu", "nomad"),
        )
        .expect("a declared format"),
        MovePlan::OfflineCopy
    );
}

#[test]
fn the_plan_is_the_strongest_requirement_any_volume_imposes() {
    let volumes = [
        volume("scratch", PortabilityTier::Ephemeral, "nomad"),
        volume("shared", PortabilityTier::SharedExternal, "s3"),
        volume("data", PortabilityTier::PortableOffline, "nomad"),
    ];
    assert_eq!(
        plan_move(
            &volumes,
            &placement("a", "docker", "nomad"),
            &placement("b", "docker", "nomad"),
        )
        .expect("a move"),
        MovePlan::OfflineCopy,
        "one portable volume among ephemeral ones still means a quiesced copy"
    );
}

#[test]
fn a_workload_does_not_move_between_architectures() {
    let from = placement("a", "docker", "nomad");
    let mut to = placement("b", "docker", "nomad");
    to.architecture = "aarch64".to_owned();
    assert_eq!(
        plan_move(&[], &from, &to),
        Err(PortabilityError::ArchitectureMismatch),
        "even a stateless workload: its image is for one architecture"
    );
}

#[test]
fn recreation_is_only_lossless_when_the_data_lives_elsewhere_or_was_disposable() {
    assert!(recreation_is_lossless(&[]));
    assert!(recreation_is_lossless(&[
        volume("scratch", PortabilityTier::Ephemeral, "nomad"),
        volume("shared", PortabilityTier::SharedExternal, "s3"),
    ]));
    assert!(
        !recreation_is_lossless(&[volume("data", PortabilityTier::ProviderLocal, "nomad")]),
        "recreating this would present an empty volume as the workload's"
    );
    assert!(!recreation_is_lossless(&[volume(
        "data",
        PortabilityTier::PortableOffline,
        "nomad"
    )]));
}

#[test]
fn a_shared_external_volume_is_reattached_not_copied() {
    assert_eq!(
        plan_move(
            &[volume("shared", PortabilityTier::SharedExternal, "s3")],
            &placement("a", "docker", "nomad"),
            &placement("b", "docker", "native-legacy"),
        )
        .expect("a move"),
        MovePlan::Reattach,
        "the data never moves, so crossing providers is fine"
    );
}
