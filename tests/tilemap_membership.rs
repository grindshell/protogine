//! Membership, transfer and per-map collision, from outside the crate.
//!
//! The evidence M2 mandates for simultaneous maps: two maps with overlapping
//! coordinate ranges and different walls giving independent results, transfer
//! that succeeds or refuses atomically, and a teleport validated against the
//! map the body is a member of rather than against any other live one.
//!
//! Every fixture here puts a body on *each* map wherever the rule is about
//! scoping. With bodies on one map only, "scan the edited map's members" and
//! "scan every collider" are the same scan, and an assertion cannot tell them
//! apart. Runs in the core configuration.

use protogine::collision::{CollisionError, TileCollider};
use protogine::kernel::{
    ColliderPlacement, EntityHandle, Kernel, KernelError, Position, TileMapHandle, Velocity,
};
use protogine::tilemap::{TileMap, TileMapInfo};

/// An 8x8 room on a `tile`-pixel grid at `origin`, solid down one column.
///
/// The column is what makes two maps disagree: a body stops at its own map's
/// wall and never sees the other's, even though both cover the same world
/// coordinates.
fn walled(column: usize, tile: u32, origin: i32) -> TileMap {
    let mut cells = vec![0u16; 64];
    for row in 0..8 {
        cells[row * 8 + column] = 1;
    }
    TileMap::new(
        TileMapInfo {
            columns: 8,
            rows: 8,
            tile_width: tile,
            tile_height: tile,
            origin_x: origin,
            origin_y: origin,
        },
        vec![true],
        cells,
    )
    .expect("test map is within the frozen limits")
}

fn square(size: f64) -> TileCollider {
    TileCollider {
        offset_x: 0.0,
        offset_y: 0.0,
        width: size,
        height: size,
    }
}

fn joining(map: &TileMapHandle, collider: TileCollider) -> Option<ColliderPlacement> {
    Some(ColliderPlacement {
        map: map.clone(),
        collider,
        position: None,
    })
}

fn moving(
    map: &TileMapHandle,
    collider: TileCollider,
    to: (f64, f64),
) -> Option<ColliderPlacement> {
    Some(ColliderPlacement {
        map: map.clone(),
        collider,
        position: Some(Position { x: to.0, y: to.1 }),
    })
}

/// A body at `at`, a member of `map`, moving at `velocity`.
fn body(
    kernel: &mut Kernel,
    map: &TileMapHandle,
    at: (f64, f64),
    velocity: (f64, f64),
) -> EntityHandle {
    let entity = kernel
        .spawn(Position { x: at.0, y: at.1 })
        .expect("spawn the body");
    kernel
        .set_tile_collider(&entity, joining(map, square(32.0)))
        .expect("the fixture start is a legal placement");
    kernel
        .set_velocity(
            &entity,
            Velocity {
                x: velocity.0,
                y: velocity.1,
            },
        )
        .expect("finite velocity");
    entity
}

#[test]
fn two_overlapping_maps_stop_their_own_bodies_at_their_own_walls() {
    // The mandated evidence, and the reason it works is M2-3: both maps cover
    // the same world coordinates, so the bodies are not separated by geometry.
    // Each consults the map it is a member of and nothing else.
    let mut kernel = Kernel::new();
    let near = kernel.create_tilemap(walled(3, 32, 0)).unwrap();
    let far = kernel.create_tilemap(walled(5, 32, 0)).unwrap();

    // Same start, same velocity, different maps.
    let on_near = body(&mut kernel, &near, (32.0, 32.0), (12_000.0, 0.0));
    let on_far = body(&mut kernel, &far, (32.0, 32.0), (12_000.0, 0.0));

    // Named rather than unwrapped: an unpaired collider refuses the whole pass
    // by name, and this is where a control that breaks the pairing lands.
    kernel
        .fixed_update()
        .expect("every collider must sweep against the map it is a member of");

    // Flush against its own wall's near face: the wall column's low edge less
    // the body's own width.
    assert_eq!(
        kernel.position(&on_near).unwrap().x,
        64.0,
        "a body on the near-walled map must stop at column 3"
    );
    assert_eq!(
        kernel.position(&on_far).unwrap().x,
        128.0,
        "while a body at the same coordinates on the far-walled map passes it"
    );
}

#[test]
fn a_transfer_moves_membership_and_position_in_one_call() {
    let mut kernel = Kernel::new();
    let here = kernel.create_tilemap(walled(3, 32, 0)).unwrap();
    // Disjoint: no world coordinate belongs to both, so reaching it at all
    // requires the position M2-5 folds into the same call.
    let away = kernel.create_tilemap(walled(3, 32, 1_000)).unwrap();
    let entity = body(&mut kernel, &here, (32.0, 32.0), (0.0, 0.0));

    assert_eq!(
        kernel.tile_collider(&entity).unwrap(),
        Some((here.clone(), square(32.0))),
        "membership is observable, which is the only way it is observable at all"
    );

    kernel
        .set_tile_collider(&entity, moving(&away, square(32.0), (1_032.0, 1_032.0)))
        .expect("a transfer carrying a position reaches a disjoint map");
    assert_eq!(
        kernel.tile_collider(&entity).unwrap(),
        Some((away.clone(), square(32.0))),
        "the body is a member of the destination"
    );
    assert_eq!(
        kernel.position(&entity).unwrap(),
        Position {
            x: 1_032.0,
            y: 1_032.0
        },
        "and it moved with the membership, in the same call"
    );

    // And back, which is the "in both directions" half of the gate.
    kernel
        .set_tile_collider(&entity, moving(&here, square(32.0), (32.0, 32.0)))
        .expect("and back again");
    assert_eq!(
        kernel.tile_collider(&entity).unwrap().unwrap().0,
        here,
        "membership returns with it"
    );
}

#[test]
fn a_refused_transfer_leaves_membership_geometry_and_position_untouched() {
    let mut kernel = Kernel::new();
    let here = kernel.create_tilemap(walled(3, 32, 0)).unwrap();
    let away = kernel.create_tilemap(walled(3, 32, 1_000)).unwrap();
    // A two-pixel grid caps a legal extent at sixteen pixels, so the body's own
    // box is illegal there even though it is legal where it stands. M2-5 puts
    // that check before the placement one deliberately.
    let fine = kernel.create_tilemap(walled(3, 2, 2_000)).unwrap();
    let retired = kernel.create_tilemap(walled(3, 32, 3_000)).unwrap();
    kernel.remove_tilemap(&retired).unwrap();

    let entity = body(&mut kernel, &here, (32.0, 32.0), (0.0, 0.0));
    let before = kernel.position(&entity).unwrap();

    for (refusal, expected, why) in [
        (
            kernel.set_tile_collider(&entity, moving(&away, square(32.0), (1_096.0, 1_032.0))),
            KernelError::Collision(CollisionError::Placement),
            "an illegal destination box",
        ),
        (
            kernel.set_tile_collider(&entity, moving(&fine, square(32.0), (2_002.0, 2_002.0))),
            KernelError::Collision(CollisionError::Extent),
            "an extent illegal against a smaller tile size",
        ),
        (
            kernel.set_tile_collider(&entity, moving(&retired, square(32.0), (3_032.0, 3_032.0))),
            KernelError::InvalidTileMap,
            "a stale destination handle",
        ),
    ] {
        assert_eq!(refusal, Err(expected), "{why} must refuse");
        assert_eq!(
            kernel.tile_collider(&entity).unwrap(),
            Some((here.clone(), square(32.0))),
            "{why} left membership and geometry alone"
        );
        assert_eq!(
            kernel.position(&entity).unwrap(),
            before,
            "{why} left the position alone"
        );
    }
}

#[test]
fn a_teleport_validates_against_the_member_map_and_no_other() {
    // M2-R2. The same sentence as T5 - "reject an overlapping or out-of-map
    // destination" - now means the body's own map, and the two maps here make
    // the difference observable rather than notional.
    let mut kernel = Kernel::new();
    // The near map is the *implicit* one deliberately: a teleport that resolved
    // the session's current map instead of the body's own would then be
    // indistinguishable from a correct one until the body transfers away, which
    // is exactly the substitution M2-R2 exists to forbid.
    let near = kernel.set_tilemap(walled(3, 32, 0)).unwrap();
    let far = kernel.create_tilemap(walled(5, 32, 0)).unwrap();
    let entity = body(&mut kernel, &near, (32.0, 32.0), (0.0, 0.0));

    // Cell (3, 1) is this map's wall and the other map's floor.
    let contested = Position { x: 96.0, y: 32.0 };
    assert_eq!(
        kernel.set_position(&entity, contested),
        Err(KernelError::Collision(CollisionError::Placement)),
        "a teleport into the member map's wall refuses"
    );
    assert_eq!(
        kernel.position(&entity).unwrap(),
        Position { x: 32.0, y: 32.0 },
        "and moved nothing"
    );

    // The same destination on the other map, reached only by transfer.
    kernel
        .set_tile_collider(&entity, joining(&far, square(32.0)))
        .unwrap();
    kernel
        .set_position(&entity, contested)
        .expect("the destination is legal on the map the body is now a member of");
    assert_eq!(kernel.position(&entity).unwrap(), contested);
}

#[test]
fn removing_a_map_leaves_unrelated_maps_and_their_bodies_running() {
    let mut kernel = Kernel::new();
    let kept = kernel.create_tilemap(walled(3, 32, 0)).unwrap();
    let doomed = kernel.create_tilemap(walled(5, 32, 0)).unwrap();
    let stays = body(&mut kernel, &kept, (32.0, 32.0), (12_000.0, 0.0));
    let leaves = body(&mut kernel, &doomed, (32.0, 32.0), (12_000.0, 0.0));

    assert_eq!(
        kernel.remove_tilemap(&doomed),
        Err(KernelError::CollidersAttached),
        "a map with a member refuses removal"
    );
    // Detaching its only member is what makes it removable, and nothing about
    // the other map's body changes.
    kernel.set_tile_collider(&leaves, None).unwrap();
    kernel
        .remove_tilemap(&doomed)
        .expect("with no members it goes");
    assert_eq!(kernel.tilemap_count(), 1);

    kernel.fixed_update().unwrap();
    assert_eq!(
        kernel.position(&stays).unwrap().x,
        64.0,
        "the surviving map still stops its own body at its own wall"
    );
    assert!(
        kernel.position(&leaves).unwrap().x > 64.0,
        "and the detached body is in free flight, not frozen"
    );
}

#[test]
fn a_collider_and_its_membership_arrive_and_leave_together() {
    // The state M2-2 makes representable, checked at every transition it has.
    // There is no public way to produce the unpaired state - both components
    // are written as one bundle - so what this can assert is that every
    // transition keeps them in step and that the read never sees one without
    // the other.
    let mut kernel = Kernel::new();
    let first = kernel.create_tilemap(walled(3, 32, 0)).unwrap();
    let second = kernel.create_tilemap(walled(5, 32, 0)).unwrap();
    let entity = body(&mut kernel, &first, (32.0, 32.0), (0.0, 0.0));
    assert_eq!(kernel.live_colliders(), 1);

    kernel
        .set_tile_collider(&entity, joining(&second, square(16.0)))
        .unwrap();
    assert_eq!(
        kernel.tile_collider(&entity).unwrap(),
        Some((second.clone(), square(16.0))),
        "a transfer replaces both halves at once"
    );
    assert_eq!(kernel.live_colliders(), 1, "and is not a new attachment");

    kernel.set_tile_collider(&entity, None).unwrap();
    assert_eq!(
        kernel.tile_collider(&entity).unwrap(),
        None,
        "detaching removes both halves"
    );
    assert_eq!(kernel.live_colliders(), 0);
    // A detached body still integrates, which is the branch an unpaired
    // collider would silently fall out of.
    kernel
        .set_velocity(&entity, Velocity { x: 60.0, y: 0.0 })
        .unwrap();
    kernel.fixed_update().unwrap();
    assert_eq!(kernel.position(&entity).unwrap().x, 33.0);

    // Despawn takes both with it, which hecs gives for free and which the
    // counter has to agree with.
    kernel
        .set_tile_collider(&entity, joining(&first, square(32.0)))
        .unwrap();
    assert_eq!(kernel.live_colliders(), 1);
    kernel.despawn(&entity).unwrap();
    assert_eq!(
        kernel.live_colliders(),
        0,
        "despawn must release the collider count as well as the components"
    );
    kernel
        .remove_tilemap(&first)
        .expect("the despawned body released its membership too");
}

#[test]
fn insertion_order_does_not_change_a_two_map_replay() {
    // M1 pinned this for one map; membership adds a second thing the sweep
    // could accidentally order by, since bodies on one map now sit together in
    // an archetype.
    let starts = [
        ((32.0, 32.0), (1_200.0, 0.0), false),
        ((32.0, 160.0), (900.0, -60.0), true),
        ((64.0, 32.0), (600.0, 300.0), false),
        ((64.0, 160.0), (1_500.0, 0.0), true),
    ];
    let mut runs = Vec::new();
    for reversed in [false, true] {
        let mut kernel = Kernel::new();
        let near = kernel.create_tilemap(walled(3, 32, 0)).unwrap();
        let far = kernel.create_tilemap(walled(5, 32, 0)).unwrap();
        let order: Vec<usize> = if reversed {
            (0..starts.len()).rev().collect()
        } else {
            (0..starts.len()).collect()
        };
        let mut placed = vec![None; starts.len()];
        for index in order {
            let (at, velocity, on_far) = starts[index];
            let map = if on_far { &far } else { &near };
            placed[index] = Some(body(&mut kernel, map, at, velocity));
        }
        for _ in 0..4 {
            kernel.fixed_update().unwrap();
        }
        runs.push(
            placed
                .into_iter()
                .map(|handle| kernel.position(&handle.unwrap()).unwrap())
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(
        runs[0], runs[1],
        "a two-map replay must not depend on the order bodies were spawned in"
    );
    // And the run is not trivially identical because nothing moved.
    assert_ne!(runs[0][0], Position { x: 32.0, y: 32.0 });
}
