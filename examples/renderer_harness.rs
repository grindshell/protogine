//! GPU harness for the shared renderer, run through
//! `tools/run_renderer_harness.ps1`.
//!
//! Unlike the Phase 0 probe, this drives the production `MacroquadRenderer`
//! and the production `AssetStore`: it reimplements no upload, validation or
//! pool logic. It exists because the pool's bounds, retirement and per-band
//! progress need a live graphics context on the main thread, which a `cargo
//! test` harness cannot provide.
//!
//! Every mode prints `PASS mode=<name>` on success and panics otherwise. The
//! `recreate` mode is a negative control and must fail at its named assertion.

use macroquad::prelude::*;
use protogine::{
    assets::{AssetStore, GpuResidency, ImageId, ImageState, JOB_QUEUE_LIMIT},
    drawing::{DrawCommand, SourceRect, Sprite},
    rendering::{BANDS_PER_PASS, MacroquadRenderer, POOL_LIMIT, RenderError, UPLOAD_PASS_BYTES},
};
use std::{fs, path::Path, time::Duration};

const WATCHDOG: Duration = Duration::from_secs(20);

/// The committed fixtures, whose pixels `tools/asset_fixtures.py` specifies
/// independently of this engine's decoder. Cycling through them varies both
/// dimensions and content across sessions.
const FIXTURES: [(&str, &[u8], u32, u32); 4] = [
    (
        "rgba",
        include_bytes!("../tests/fixtures/assets/rgba.png"),
        2,
        3,
    ),
    (
        "rgb",
        include_bytes!("../tests/fixtures/assets/rgb.png"),
        3,
        1,
    ),
    (
        "interlaced",
        include_bytes!("../tests/fixtures/assets/interlaced.png"),
        9,
        9,
    ),
    (
        "one_pixel",
        include_bytes!("../tests/fixtures/assets/one_pixel.png"),
        1,
        1,
    ),
];
const SHEET: &[u8] = include_bytes!("../examples/kenney_1-bit-pack_transparent-packed.png");

/// A bundle holding `POOL_LIMIT` fixture images plus the Kenney sheet.
fn bundle(root: &Path) {
    fs::create_dir_all(root).unwrap();
    for index in 0..POOL_LIMIT {
        let (_, bytes, _, _) = FIXTURES[index % FIXTURES.len()];
        fs::write(root.join(format!("image{index}.png")), bytes).unwrap();
    }
    fs::write(root.join("sheet.png"), SHEET).unwrap();
}

fn expected(index: usize) -> (u32, u32) {
    let (_, _, width, height) = FIXTURES[index % FIXTURES.len()];
    (width, height)
}

/// Read one screen pixel by its top-left coordinates. Macroquad's framebuffer
/// readback is bottom-up, as the Player's PNG writer also has to account for.
fn screen_pixel(screen: &Image, x: u32, y: u32) -> [u8; 4] {
    let (width, height) = (u32::from(screen.width), u32::from(screen.height));
    assert!(x < width && y < height, "sample {x},{y} is off screen");
    let index = (((height - 1 - y) * width + x) * 4) as usize;
    screen.bytes[index..index + 4].try_into().unwrap()
}

/// One whole-image sprite and one 1x1 crop of the 2x3 fixture, drawn at 16x.
fn fixture_commands(image: ImageId) -> [DrawCommand; 2] {
    let base = Sprite {
        image,
        source: SourceRect {
            x: 0,
            y: 0,
            width: 2,
            height: 3,
        },
        x: 0.0,
        y: 0.0,
        width: 32.0,
        height: 48.0,
        flip_x: false,
        flip_y: false,
        tint: [1.0; 4],
    };
    [
        DrawCommand::Sprite(base),
        DrawCommand::Sprite(Sprite {
            source: SourceRect {
                x: 1,
                y: 2,
                width: 1,
                height: 1,
            },
            x: 48.0,
            y: 0.0,
            width: 16.0,
            height: 16.0,
            ..base
        }),
    ]
}

/// Load `count` images to CPU readiness through the production store, in
/// batches the eight-job queue can admit.
fn load(store: &mut AssetStore, count: usize) -> Vec<ImageId> {
    let mut ids = Vec::new();
    for batch in (0..count).collect::<Vec<_>>().chunks(JOB_QUEUE_LIMIT) {
        for index in batch {
            ids.push(store.request_png(&format!("image{index}.png")).unwrap());
        }
        assert!(store.drain(WATCHDOG), "CPU load did not settle");
    }
    for id in &ids {
        assert_eq!(store.status(*id).unwrap().state, ImageState::Ready);
    }
    store.take_settled();
    ids
}

/// Service upload passes until nothing is outstanding, asserting the frozen
/// per-pass bounds on the way.
fn drain_uploads(
    renderer: &mut MacroquadRenderer,
    store: &AssetStore,
) -> Vec<(ImageId, GpuResidency)> {
    let mut acks = Vec::new();
    let mut passes = 0;
    while renderer.pending_uploads() > 0 {
        let before = renderer.counters();
        acks.extend(
            renderer
                .service_uploads(store)
                .expect("upload service")
                .into_iter()
                .map(|ack| (ack.id, ack.residency)),
        );
        let after = renderer.counters();
        assert!(
            after.bands - before.bands <= BANDS_PER_PASS as u64,
            "pass moved {} bands, over the {BANDS_PER_PASS} allowance",
            after.bands - before.bands
        );
        assert!(
            after.band_bytes - before.band_bytes <= UPLOAD_PASS_BYTES as u64,
            "pass moved {} bytes, over the {UPLOAD_PASS_BYTES} allowance",
            after.band_bytes - before.band_bytes
        );
        assert!(
            after.allocations - before.allocations <= 1,
            "more than one destination allocation in a pass"
        );
        passes += 1;
        assert!(passes < 10_000, "uploads never settled");
    }
    acks
}

/// One hundred attach/load/retire cycles under a single graphics context.
async fn cycles(root: &Path) {
    let mut renderer = MacroquadRenderer::new();
    let mut peak_pool = 0;
    let mut creations_at_peak = 0;
    let mut attaches = 0;
    for cycle in 0..100 {
        let count = [0, 1, POOL_LIMIT][cycle % 3];
        let mut store = AssetStore::new(root).unwrap();
        renderer.attach(store.session());
        attaches += 1;
        let ids = load(&mut store, count);

        // Staggered admission: half up front, the rest once uploads have begun.
        let (first, rest) = ids.split_at(count / 2);
        for id in first {
            renderer.admit(*id);
        }
        if !first.is_empty() {
            renderer.service_uploads(&store).unwrap();
        }
        for id in rest {
            renderer.admit(*id);
        }
        drain_uploads(&mut renderer, &store);
        assert_eq!(renderer.resident_images(), count, "cycle {cycle}");

        // Every resident image has its own slot at its own dimensions.
        let sizes = renderer.slot_sizes();
        for index in 0..count {
            assert!(
                sizes.contains(&expected(index)),
                "cycle {cycle}: no slot holds image {index}"
            );
        }
        // Drawing is what pins slots, so exercise it before retiring.
        let commands: Vec<DrawCommand> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let (width, height) = expected(index);
                DrawCommand::Sprite(Sprite {
                    image: *id,
                    source: SourceRect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    },
                    x: (index % 32) as f32 * 8.0,
                    y: (index / 32) as f32 * 8.0,
                    width: 8.0,
                    height: 8.0,
                    flip_x: false,
                    flip_y: false,
                    tint: [1.0; 4],
                })
            })
            .collect();
        clear_background(BLACK);
        renderer.render(&store, &commands).unwrap();
        next_frame().await;

        // Old identities must stop working once the session is retired.
        let stale = ids.first().copied();
        store.shutdown();
        renderer.retire();
        if let Some(stale) = stale {
            let fresh = AssetStore::new(root).unwrap();
            renderer.attach(fresh.session());
            attaches += 1;
            assert_eq!(
                renderer.validate(
                    &fresh,
                    &[DrawCommand::Sprite(Sprite {
                        image: stale,
                        source: SourceRect {
                            x: 0,
                            y: 0,
                            width: 1,
                            height: 1
                        },
                        x: 0.0,
                        y: 0.0,
                        width: 1.0,
                        height: 1.0,
                        flip_x: false,
                        flip_y: false,
                        tint: [1.0; 4],
                    })]
                ),
                Err(RenderError::ForeignImage(stale)),
                "a retired session's identity must not be inherited"
            );
            renderer.retire();
        }

        // Retirement leaves nothing of the previous game behind.
        assert!(
            renderer.slot_sizes().iter().all(|size| *size == (1, 1)),
            "cycle {cycle}: a retired slot is not 1x1"
        );
        assert!(
            renderer.identities_stable(),
            "cycle {cycle}: a slot changed backend identity"
        );
        let counters = renderer.counters();
        assert!(
            counters.creations <= POOL_LIMIT as u64,
            "cycle {cycle}: {} lifetime creations exceed {POOL_LIMIT}",
            counters.creations
        );
        assert_eq!(counters.creations as usize, renderer.pool_size());
        if renderer.pool_size() > peak_pool {
            peak_pool = renderer.pool_size();
            creations_at_peak = counters.creations;
        } else {
            assert_eq!(
                counters.creations, creations_at_peak,
                "cycle {cycle}: the pool created a texture after reaching {peak_pool}"
            );
        }
    }
    let (payload, transparent) = renderer.measure_payload();
    assert!(transparent, "a retired slot still holds opaque content");
    assert_eq!(
        payload,
        peak_pool * 4,
        "retired slots must hold one transparent texel each"
    );
    let counters = renderer.counters();
    assert_eq!(peak_pool, POOL_LIMIT);
    assert_eq!(counters.creations, POOL_LIMIT as u64);
    assert_eq!(counters.sessions, attaches);
    // Every admitted image was allocated and completed exactly once.
    assert_eq!(counters.admitted, counters.allocations);
    assert_eq!(counters.admitted, counters.completed);
    println!(
        "PASS mode=cycles pool={peak_pool} creations={} sessions={} images={} bands={} retired_payload={payload}",
        counters.creations, counters.sessions, counters.completed, counters.bands
    );
}

/// Per-band progress on an image large enough to need many passes.
async fn bands(root: &Path) {
    let mut store = AssetStore::new(root).unwrap();
    let mut renderer = MacroquadRenderer::new();
    renderer.attach(store.session());
    let id = store.request_png("sheet.png").unwrap();
    assert!(store.drain(WATCHDOG), "the sheet did not load");
    store.take_settled();
    let data = store.image(id).expect("ready pixels");
    assert_eq!((data.width(), data.height()), (784, 352));

    let sprite = DrawCommand::Sprite(Sprite {
        image: id,
        source: SourceRect {
            x: 0,
            y: 0,
            width: 784,
            height: 352,
        },
        x: 0.0,
        y: 0.0,
        width: 784.0,
        height: 352.0,
        flip_x: false,
        flip_y: false,
        tint: [1.0; 4],
    });

    renderer.admit(id);
    let mut passes = 0;
    let mut residencies = Vec::new();
    while renderer.pending_uploads() > 0 {
        let before = renderer.counters();
        for ack in renderer.service_uploads(&store).unwrap() {
            residencies.push(ack.residency);
        }
        let after = renderer.counters();
        assert!(after.bands - before.bands <= BANDS_PER_PASS as u64);
        assert!(after.band_bytes - before.band_bytes <= UPLOAD_PASS_BYTES as u64);
        passes += 1;
        // A partially transferred image is never drawable, and validating it
        // must not force the remaining bands.
        let bands_before = renderer.counters().bands;
        renderer
            .validate(&store, std::slice::from_ref(&sprite))
            .unwrap();
        assert_eq!(
            renderer.counters().bands,
            bands_before,
            "validation advanced an upload"
        );
        if renderer.pending_uploads() > 0 {
            assert_eq!(
                renderer.resident_images(),
                0,
                "an incomplete image became drawable"
            );
        }
        clear_background(BLACK);
        next_frame().await;
    }
    assert!(
        passes > 1,
        "a 784x352 image must need more than one bounded pass"
    );
    assert_eq!(residencies, [GpuResidency::Pending, GpuResidency::Resident]);
    assert_eq!(renderer.resident_images(), 1);

    // Drawing a resident image performs no upload work at all.
    let before = renderer.counters();
    clear_background(BLACK);
    renderer
        .render(&store, std::slice::from_ref(&sprite))
        .unwrap();
    next_frame().await;
    let after = renderer.counters();
    assert_eq!(
        (after.bands, after.band_bytes, after.allocations),
        (before.bands, before.band_bytes, before.allocations),
        "the draw traversal moved upload work"
    );
    assert_eq!(after.drawn - before.drawn, 1);

    // Servicing again uploads nothing: a completed image is not re-transferred.
    renderer.admit(id);
    renderer.service_uploads(&store).unwrap();
    assert_eq!(renderer.counters().bands, after.bands);
    println!(
        "PASS mode=bands passes={passes} bands={} bytes={}",
        after.bands, after.band_bytes
    );
}

/// An upload cancelled mid-transfer releases its slot and reuses it.
async fn eviction(root: &Path) {
    let mut store = AssetStore::new(root).unwrap();
    let mut renderer = MacroquadRenderer::new();
    renderer.attach(store.session());
    let sheet = store.request_png("sheet.png").unwrap();
    assert!(store.drain(WATCHDOG), "the sheet did not load");
    store.take_settled();

    renderer.admit(sheet);
    renderer.service_uploads(&store).unwrap();
    renderer.service_uploads(&store).unwrap();
    assert_eq!(renderer.pending_uploads(), 1, "the upload should be staged");
    assert_eq!(renderer.pool_size(), 1);
    let staged = renderer.slot_sizes();
    assert_eq!(staged[0], (784, 352), "destination storage is allocated");

    // Unloading mid-transfer is the real upload failure: the CPU content is
    // gone, so the remaining bands can never arrive.
    assert!(store.unload(sheet));
    let acks = renderer.service_uploads(&store).unwrap();
    assert_eq!(
        acks.iter().map(|ack| ack.residency).collect::<Vec<_>>(),
        [GpuResidency::Released]
    );
    assert_eq!(renderer.pending_uploads(), 0);
    assert_eq!(renderer.resident_images(), 0);
    assert_eq!(
        renderer.slot_sizes()[0],
        (1, 1),
        "a cancelled upload must clear its slot"
    );
    let cancelled = renderer.counters();
    assert_eq!(cancelled.cancelled, 1);
    assert_eq!(cancelled.completed, 0);

    // Reuse the freed slot at a different size. This is where a stale
    // allocation or stale source UVs would show, because Macroquad reads the
    // backend record's dimensions for both the texture size and the UVs.
    store.take_settled();
    let small = store.request_png("image0.png").unwrap();
    assert_ne!(small, sheet, "a fresh request is a new identity");
    assert!(store.drain(WATCHDOG));
    store.take_settled();
    renderer.admit(small);
    drain_uploads(&mut renderer, &store);
    assert_eq!(renderer.pool_size(), 1, "reuse created a second slot");
    assert_eq!(renderer.counters().creations, 1);
    assert_eq!(renderer.counters().completed, 1);
    assert_eq!(
        renderer.slot_sizes()[0],
        (2, 3),
        "the reused slot kept its old allocation"
    );
    assert!(renderer.identities_stable());

    // Draw through the production renderer and read the framebuffer back. The
    // 2x3 fixture's pixels are specified independently of this engine, so these
    // are expected values rather than a recording of whatever was produced.
    clear_background(BLACK);
    renderer.render(&store, &fixture_commands(small)).unwrap();
    let screen = get_screen_data();
    assert_eq!(
        screen_pixel(&screen, 8, 8),
        [255, 0, 0, 255],
        "reused slot: top-left texel"
    );
    assert_eq!(
        screen_pixel(&screen, 24, 24),
        [255, 255, 0, 255],
        "reused slot: middle-right texel"
    );
    assert_eq!(
        screen_pixel(&screen, 24, 40),
        [255, 255, 255, 255],
        "reused slot: bottom-right texel"
    );
    // A 1x1 crop resolves against the new 2x3 allocation, not the old 784x352
    // one, which is the source-UV half of the reuse contract.
    assert_eq!(
        screen_pixel(&screen, 56, 8),
        [255, 255, 255, 255],
        "reused slot: source UVs still resolve against the old size"
    );
    next_frame().await;
    renderer.retire();

    // A 1x1 upload has the dimensions of an idle slot but contains an opaque
    // pixel. Unload while its draw is queued: the texel must survive readback,
    // then become transparent when the deferred slot is released.
    store.shutdown();
    let mut store = AssetStore::new(root).unwrap();
    renderer.attach(store.session());
    let pixel = store.request_png("image3.png").unwrap();
    assert!(store.drain(WATCHDOG));
    store.take_settled();
    renderer.admit(pixel);
    drain_uploads(&mut renderer, &store);
    assert_eq!(renderer.measure_payload(), (4, false));
    let command = DrawCommand::Sprite(Sprite {
        image: pixel,
        source: SourceRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        x: 0.0,
        y: 0.0,
        width: 16.0,
        height: 16.0,
        flip_x: false,
        flip_y: false,
        tint: [1.0; 4],
    });
    renderer.render(&store, &[command]).unwrap();
    assert!(store.unload(pixel));
    renderer.service_uploads(&store).unwrap();
    assert_eq!(screen_pixel(&get_screen_data(), 8, 8), [255, 0, 0, 255]);
    next_frame().await;
    renderer.service_uploads(&store).unwrap();
    assert_eq!(
        renderer.measure_payload(),
        (4, true),
        "unloaded 1x1 texel was not cleared"
    );
    assert_eq!(renderer.counters().creations, 1);

    // A fault can destroy a store midway through an upload. The renderer then
    // owns the last service-side CPU pin as well as the GPU allocation; retire
    // must release both, even though the upload never became resident.
    store.take_settled();
    let sheet = store.request_png("sheet.png").unwrap();
    assert!(store.drain(WATCHDOG));
    let pixels = store.image(sheet).unwrap();
    renderer.admit(sheet);
    renderer.service_uploads(&store).unwrap();
    assert_eq!(renderer.pending_uploads(), 1);
    drop(store);
    assert_eq!(std::sync::Arc::strong_count(&pixels), 2);
    renderer.retire();
    assert_eq!(
        std::sync::Arc::strong_count(&pixels),
        1,
        "retirement retained upload pixels"
    );
    assert_eq!(renderer.pending_uploads(), 0);
    assert_eq!(renderer.resident_images(), 0);
    assert_eq!(renderer.measure_payload(), (4, true));
    assert!(renderer.identities_stable());
    println!(
        "PASS mode=eviction creations={} cancelled={} completed={} reused_size=2x3",
        renderer.counters().creations,
        cancelled.cancelled,
        renderer.counters().completed
    );
}

/// A slot the queued frame still draws from must not be recycled, and the
/// resulting pressure must yield rather than fault.
async fn pressure(root: &Path) {
    let mut store = AssetStore::new(root).unwrap();
    let mut renderer = MacroquadRenderer::new();
    renderer.attach(store.session());
    let ids = load(&mut store, POOL_LIMIT);
    for id in &ids {
        renderer.admit(*id);
    }
    drain_uploads(&mut renderer, &store);
    assert_eq!(renderer.pool_size(), POOL_LIMIT);
    assert_eq!(renderer.resident_images(), POOL_LIMIT);

    // Draw every image, so every slot is pinned by the queued frame.
    let commands: Vec<DrawCommand> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            let (width, height) = expected(index);
            DrawCommand::Sprite(Sprite {
                image: *id,
                source: SourceRect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
                x: (index % 32) as f32 * 8.0,
                y: (index / 32) as f32 * 8.0,
                width: 8.0,
                height: 8.0,
                flip_x: false,
                flip_y: false,
                tint: [1.0; 4],
            })
        })
        .collect();
    clear_background(BLACK);
    renderer.render(&store, &commands).unwrap();

    // Evict one and request another before the frame is presented. The freed
    // slot is deferred behind the queued frame, so the pool has nothing to
    // hand out this pass and must yield instead of failing the session.
    assert!(store.unload(ids[0]));
    store.take_settled();
    let replacement = store.request_png("sheet.png").unwrap();
    assert!(store.drain(WATCHDOG));
    store.take_settled();
    renderer.admit(replacement);
    let acks = renderer
        .service_uploads(&store)
        .expect("pressure must yield");
    assert!(
        acks.iter()
            .any(|ack| ack.id == ids[0] && ack.residency == GpuResidency::Released),
        "the evicted image should be released"
    );
    assert_eq!(
        renderer.pending_uploads(),
        1,
        "the admission must survive the yield"
    );
    assert_eq!(renderer.counters().allocations, POOL_LIMIT as u64);

    // The next pass releases the deferred slot and the upload proceeds.
    next_frame().await;
    drain_uploads(&mut renderer, &store);
    assert_eq!(
        renderer.pool_size(),
        POOL_LIMIT,
        "the pool grew past its cap"
    );
    assert_eq!(renderer.counters().creations, POOL_LIMIT as u64);
    assert_eq!(renderer.resident_images(), POOL_LIMIT);
    println!(
        "PASS mode=pressure creations={} allocations={}",
        renderer.counters().creations,
        renderer.counters().allocations
    );
}

/// Negative control: a pool that recreates textures must fail the lifetime
/// creation assertion the other modes rely on.
async fn recreate(root: &Path) {
    let mut store = AssetStore::new(root).unwrap();
    let mut renderer = MacroquadRenderer::new();
    renderer.attach(store.session());
    let ids = load(&mut store, POOL_LIMIT);
    for id in &ids {
        renderer.admit(*id);
    }
    drain_uploads(&mut renderer, &store);
    let creations = renderer.counters().creations;
    assert_eq!(creations, POOL_LIMIT as u64);

    // Stand in for a renderer that recreated its pool each session instead of
    // resizing in place. The assertion the other modes make must reject it.
    let mut recreated = Vec::new();
    for _ in 0..POOL_LIMIT {
        recreated.push(Texture2D::from_rgba8(1, 1, &[0; 4]));
    }
    let total = creations + recreated.len() as u64;
    clear_background(BLACK);
    next_frame().await;
    assert!(
        total <= POOL_LIMIT as u64,
        "negative control: {total} lifetime texture creations exceed {POOL_LIMIT}"
    );
    println!("FAIL mode=recreate: the control did not trip");
}

async fn run(mode: String, root: std::path::PathBuf) {
    bundle(&root);
    // The store requires an absolute root, exactly as a shipped bundle does.
    let root = fs::canonicalize(&root).expect("bundle root");
    match mode.as_str() {
        "cycles" => cycles(&root).await,
        "bands" => bands(&root).await,
        "eviction" => eviction(&root).await,
        "pressure" => pressure(&root).await,
        "recreate" => recreate(&root).await,
        other => panic!("unknown mode {other}"),
    }
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "cycles".into());
    let root = std::env::args()
        .nth(2)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("target/renderer-harness/bundle"));
    macroquad::Window::from_config(
        Conf {
            window_title: "Protogine renderer harness".to_owned(),
            window_width: 320,
            window_height: 200,
            window_resizable: false,
            ..Default::default()
        },
        async move { run(mode, root).await },
    );
}
