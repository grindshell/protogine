use protogine::kernel::{ENTITY_LIMIT, Kernel, KernelError, Position, Velocity};

#[test]
fn handles_reject_other_sessions_despawn_and_slot_reuse() {
    let mut a = Kernel::new();
    let mut b = Kernel::new();
    let old = a.spawn(Position::default()).unwrap();
    let foreign = b.spawn(Position::default()).unwrap();
    assert_ne!(old, foreign);
    assert_eq!(b.position(&old), Err(KernelError::InvalidHandle));
    assert_eq!(b.despawn(&old), Err(KernelError::InvalidHandle));
    a.despawn(&old).unwrap();
    let replacement = a.spawn(Position { x: 42.0, y: 0.0 }).unwrap();
    assert_ne!(old, replacement);
    assert_eq!(a.position(&old), Err(KernelError::InvalidHandle));
    assert_eq!(
        a.set_velocity(&old, Velocity::default()),
        Err(KernelError::InvalidHandle)
    );
    assert_eq!(a.position(&replacement).unwrap().x, 42.0);
    a.stop();
    assert_eq!(a.position(&replacement), Err(KernelError::Inactive));
    assert_eq!(a.spawn(Position::default()), Err(KernelError::Inactive));
    assert_eq!(a.fixed_update(), Err(KernelError::Inactive));
    assert_eq!(a.entities(), Err(KernelError::Inactive));
    assert!(b.position(&foreign).is_ok());
}

#[test]
fn snapshots_are_owned_ordered_and_safe_to_mutate_around() {
    let mut kernel = Kernel::new();
    let a = kernel.spawn(Position { x: 10.0, y: 20.0 }).unwrap();
    let b = kernel.spawn(Position { x: 30.0, y: 40.0 }).unwrap();
    let before = kernel.snapshot().unwrap();
    assert_eq!(kernel.entities().unwrap(), [a.clone(), b.clone()]);
    kernel
        .set_velocity(&a, Velocity { x: 60.0, y: -120.0 })
        .unwrap();
    kernel.fixed_update().unwrap();
    assert_eq!(kernel.position(&a).unwrap(), Position { x: 11.0, y: 18.0 });
    assert_eq!(before[0].position, Position { x: 10.0, y: 20.0 });
    for entity in kernel.entities().unwrap() {
        kernel.despawn(&entity).unwrap();
    }
    assert!(kernel.snapshot().unwrap().is_empty());
    assert_eq!(before.len(), 2);
}

#[test]
fn nonfinite_arguments_and_system_overflow_preserve_existing_state() {
    let mut kernel = Kernel::new();
    let a = kernel.spawn(Position::default()).unwrap();
    let b = kernel
        .spawn(Position {
            x: f64::MAX,
            y: 0.0,
        })
        .unwrap();
    for n in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            kernel.spawn(Position { x: n, y: 0.0 }),
            Err(KernelError::Nonfinite)
        );
        assert_eq!(
            kernel.set_position(&a, Position { x: 0.0, y: n }),
            Err(KernelError::Nonfinite)
        );
        assert_eq!(
            kernel.set_velocity(&a, Velocity { x: n, y: 0.0 }),
            Err(KernelError::Nonfinite)
        );
    }
    kernel
        .set_velocity(&a, Velocity { x: 60.0, y: 0.0 })
        .unwrap();
    kernel
        .set_velocity(
            &b,
            Velocity {
                x: f64::MAX,
                y: 0.0,
            },
        )
        .unwrap();
    let before = kernel.snapshot().unwrap();
    assert_eq!(kernel.fixed_update(), Err(KernelError::Nonfinite));
    assert_eq!(kernel.snapshot().unwrap(), before);
}

#[test]
fn live_entity_limit_releases_capacity_after_despawn() {
    let mut kernel = Kernel::new();
    for _ in 0..ENTITY_LIMIT {
        kernel.spawn(Position::default()).unwrap();
    }
    assert_eq!(
        kernel.spawn(Position::default()),
        Err(KernelError::EntityLimit)
    );
    let first = kernel.entities().unwrap().remove(0);
    kernel.despawn(&first).unwrap();
    assert!(kernel.spawn(Position::default()).is_ok());
    assert_eq!(kernel.entities().unwrap().len(), ENTITY_LIMIT as usize);
}
