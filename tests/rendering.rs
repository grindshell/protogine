#![cfg(feature = "graphics")]

//! Command validation and admission bookkeeping for the shared renderer.
//!
//! Everything here runs without a graphics context: `MacroquadRenderer::new`
//! allocates nothing, `attach` on an empty pool touches no texture, and
//! `validate` is the check `render` performs before it queues anything. Upload
//! passes, pixels, pool reuse and retirement need a live context and are
//! covered by the GPU harness instead.

use protogine::{
    assets::{AssetStore, ImageId, ImageState},
    drawing::{DRAW_COMMAND_LIMIT, DrawCommand, SourceRect, Sprite},
    rendering::{MacroquadRenderer, RenderError},
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const WATCHDOG: Duration = Duration::from_secs(10);

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/assets/{name}.png"))
}

/// A bundle holding one committed fixture as `art.png`.
fn bundle(name: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::copy(fixture(name), root.path().join("art.png")).unwrap();
    root
}

/// A store whose single `art.png` has settled, plus its identity.
fn loaded(name: &str) -> (tempfile::TempDir, AssetStore, ImageId) {
    let root = bundle(name);
    let mut store = AssetStore::new(root.path()).unwrap();
    let id = store.request_png("art.png").unwrap();
    assert!(store.drain(WATCHDOG), "{name} did not settle");
    (root, store, id)
}

/// The 2x3 fixture drawn whole, which every negative case below perturbs.
fn sprite(image: ImageId) -> Sprite {
    Sprite {
        image,
        source: SourceRect {
            x: 0,
            y: 0,
            width: 2,
            height: 3,
        },
        x: 0.0,
        y: 0.0,
        width: 2.0,
        height: 3.0,
        flip_x: false,
        flip_y: false,
        tint: [1.0; 4],
    }
}

fn attached(store: &AssetStore) -> MacroquadRenderer {
    let mut renderer = MacroquadRenderer::new();
    renderer.attach(store.session());
    renderer
}

#[test]
fn a_detached_renderer_refuses_drawing_and_upload_service() {
    let (_root, mut store, _id) = loaded("rgba");
    let mut renderer = MacroquadRenderer::new();
    assert_eq!(renderer.session(), None);
    assert_eq!(renderer.validate(&store, &[]), Err(RenderError::Detached));
    assert_eq!(
        renderer.service_uploads(&store).unwrap_err(),
        RenderError::Detached
    );

    // Attaching binds one session, and a store from any other is refused even
    // when a session is attached.
    renderer.attach(store.session());
    assert_eq!(renderer.session(), Some(store.session()));
    assert_eq!(renderer.validate(&store, &[]), Ok(()));
    let other = AssetStore::new(bundle("rgba").path()).unwrap();
    assert_eq!(
        renderer.validate(&other, &[]),
        Err(RenderError::Detached),
        "a renderer must not serve a store it is not attached to"
    );
    // Retirement is idempotent and unbinds the session.
    renderer.retire();
    renderer.retire();
    assert_eq!(renderer.session(), None);
    assert_eq!(renderer.validate(&store, &[]), Err(RenderError::Detached));
    store.shutdown();
}

#[test]
fn validation_refuses_foreign_unknown_and_undrawable_images() {
    let (_root, mut store, ready) = loaded("rgba");
    let renderer = attached(&store);
    assert_eq!(
        renderer.validate(&store, &[DrawCommand::Sprite(sprite(ready))]),
        Ok(())
    );

    // A handle from another store, with the same image number.
    let (_other_root, mut other, foreign) = loaded("rgba");
    assert_eq!(foreign.number(), ready.number());
    assert_eq!(
        renderer.validate(&store, &[DrawCommand::Sprite(sprite(foreign))]),
        Err(RenderError::ForeignImage(foreign))
    );

    // A failed job is inspectable on the store but never drawable.
    let (_broken_root, mut broken, failed) = loaded("wrong_format");
    let broken_renderer = attached(&broken);
    assert_eq!(
        broken.status(failed).unwrap().state,
        ImageState::Failed,
        "the fixture must fail rather than decode"
    );
    assert_eq!(
        broken_renderer.validate(&broken, &[DrawCommand::Sprite(sprite(failed))]),
        Err(RenderError::NotDrawable(failed))
    );

    // Once the host drains the terminal transition, the store forgets it.
    broken.take_settled();
    assert_eq!(
        broken_renderer.validate(&broken, &[DrawCommand::Sprite(sprite(failed))]),
        Err(RenderError::UnknownImage(failed))
    );

    // The same holds for an image the script unloaded.
    assert!(store.unload(ready));
    assert_eq!(
        renderer.validate(&store, &[DrawCommand::Sprite(sprite(ready))]),
        Err(RenderError::NotDrawable(ready))
    );
    store.take_settled();
    assert_eq!(
        renderer.validate(&store, &[DrawCommand::Sprite(sprite(ready))]),
        Err(RenderError::UnknownImage(ready))
    );
    for store in [&mut store, &mut other, &mut broken] {
        store.shutdown();
    }
}

#[test]
fn validation_refuses_commands_outside_the_shared_scalar_contract() {
    let (_root, mut store, id) = loaded("rgba");
    let renderer = attached(&store);
    let refused = |command: DrawCommand| {
        let error = renderer
            .validate(&store, &[command])
            .expect_err("an invalid command was accepted");
        assert!(
            matches!(error, RenderError::InvalidCommand(_)),
            "{error:?} should be an invalid command"
        );
    };

    // Rust-supplied commands are validated exactly like published ones.
    refused(DrawCommand::Clear([1.0, 1.0, 1.0, 1.5]));
    refused(DrawCommand::Clear([f32::NAN, 1.0, 1.0, 1.0]));
    refused(DrawCommand::Rect {
        x: 2e6,
        y: 0.0,
        width: 1.0,
        height: 1.0,
        color: [1.0; 4],
    });
    refused(DrawCommand::Rect {
        x: 0.0,
        y: 0.0,
        width: -1.0,
        height: 1.0,
        color: [1.0; 4],
    });
    refused(DrawCommand::Sprite(Sprite {
        tint: [1.0, 1.0, 1.0, -0.5],
        ..sprite(id)
    }));
    refused(DrawCommand::Sprite(Sprite {
        x: f32::INFINITY,
        ..sprite(id)
    }));
    refused(DrawCommand::Sprite(Sprite {
        width: -1.0,
        ..sprite(id)
    }));
    // A zero-extent source cannot be published by the bindings, and the
    // renderer refuses it rather than sampling an empty region.
    refused(DrawCommand::Sprite(Sprite {
        source: SourceRect {
            x: 0,
            y: 0,
            width: 0,
            height: 3,
        },
        ..sprite(id)
    }));
    // Half-open extents must fit the 2x3 image.
    refused(DrawCommand::Sprite(Sprite {
        source: SourceRect {
            x: 1,
            y: 0,
            width: 2,
            height: 3,
        },
        ..sprite(id)
    }));
    refused(DrawCommand::Sprite(Sprite {
        source: SourceRect {
            x: 0,
            y: 1,
            width: 2,
            height: 3,
        },
        ..sprite(id)
    }));

    // Zero destination area is a valid no-op, as it is for rectangles.
    assert_eq!(
        renderer.validate(
            &store,
            &[DrawCommand::Sprite(Sprite {
                width: 0.0,
                height: 0.0,
                ..sprite(id)
            })]
        ),
        Ok(())
    );
    // The far edges of the source remain valid.
    assert_eq!(
        renderer.validate(
            &store,
            &[DrawCommand::Sprite(Sprite {
                source: SourceRect {
                    x: 1,
                    y: 2,
                    width: 1,
                    height: 1,
                },
                ..sprite(id)
            })]
        ),
        Ok(())
    );
    store.shutdown();
}

#[test]
fn validation_enforces_the_published_command_cap() {
    let (_root, mut store, id) = loaded("rgba");
    let renderer = attached(&store);
    let at_cap = vec![DrawCommand::Sprite(sprite(id)); DRAW_COMMAND_LIMIT];
    assert_eq!(renderer.validate(&store, &at_cap), Ok(()));
    let mut over = at_cap;
    over.push(DrawCommand::Clear([0.0, 0.0, 0.0, 1.0]));
    assert_eq!(
        renderer.validate(&store, &over),
        Err(RenderError::CommandLimit(DRAW_COMMAND_LIMIT + 1))
    );
    store.shutdown();
}

#[test]
fn a_ready_image_with_no_completed_upload_is_skipped_rather_than_refused() {
    let (_root, mut store, id) = loaded("rgba");
    let renderer = attached(&store);
    // Nothing has been admitted, let alone transferred, so the image is
    // CPU-ready with no texture. Drawing must not refuse it, and must not
    // force the upload to happen as a side effect of the traversal.
    assert_eq!(
        renderer.validate(&store, &[DrawCommand::Sprite(sprite(id))]),
        Ok(())
    );
    assert_eq!(renderer.resident_images(), 0);
    assert_eq!(renderer.counters().bands, 0);
    assert_eq!(renderer.counters().allocations, 0);
    store.shutdown();
}

#[test]
fn admission_is_once_per_image_and_ignores_other_sessions() {
    let (_root, mut store, id) = loaded("rgba");
    let (_other_root, mut other, foreign) = loaded("rgba");
    let mut renderer = attached(&store);
    assert_eq!(renderer.pending_uploads(), 0);

    renderer.admit(id);
    renderer.admit(id);
    assert_eq!(
        renderer.pending_uploads(),
        1,
        "one ready transition is one upload sequence"
    );
    // An image from another session is not this renderer's to upload.
    renderer.admit(foreign);
    assert_eq!(renderer.pending_uploads(), 1);

    // Attaching clears the mapping, so no session inherits another's work.
    renderer.attach(other.session());
    assert_eq!(renderer.pending_uploads(), 0);
    assert_eq!(renderer.counters().sessions, 2);
    // No texture was ever created: admission alone allocates nothing.
    assert_eq!(renderer.counters().creations, 0);
    assert_eq!(renderer.pool_size(), 0);
    for store in [&mut store, &mut other] {
        store.shutdown();
    }
}
