//! Phase 0 dependency probe, not the asset service or a copy of the Player renderer.
//! Run under tools/run_png_sprite_probe.ps1 for independent process timeouts.

use image::{ColorType, ImageDecoder, ImageEncoder, codecs::png::PngDecoder};
use macroquad::prelude::*;
use std::{
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

// Reuse the actual Player's orientation/PNG writer, not a second capture path.
#[allow(dead_code)]
#[path = "../src/bin/player/capture.rs"]
mod capture;

const QUANTUM: usize = 32 * 1024;
const ENCODED_LIMIT: usize = 17 * 1024 * 1024;
type ProbeResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn measure<T>(name: &str, work: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = work();
    println!("stage={name} us={}", start.elapsed().as_micros());
    result
}

fn pixels(width: usize, height: usize) -> Vec<u8> {
    let mut state = 0x0123_4567_89ab_cdef_u64;
    (0..width * height * 4)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

fn encoded(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut bytes,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::NoFilter,
    )
    .write_image(pixels, width, height, ColorType::Rgba8)
    .unwrap();
    bytes
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(bytes);
    let mut crc = u32::MAX;
    for byte in kind.iter().chain(bytes) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    out.extend_from_slice(&(!crc).to_be_bytes());
}

// Independent Adam7 scan construction and stored DEFLATE blocks. Expected pixels
// come from `pixels`, not from recording the decoder's first output.
fn interlaced(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut raw = Vec::new();
    for (x0, y0, dx, dy) in [
        (0, 0, 8, 8),
        (4, 0, 8, 8),
        (0, 4, 4, 8),
        (2, 0, 4, 4),
        (0, 2, 2, 4),
        (1, 0, 2, 2),
        (0, 1, 1, 2),
    ] {
        if x0 >= width {
            continue;
        }
        for y in (y0..height).step_by(dy) {
            raw.push(0);
            for x in (x0..width).step_by(dx) {
                raw.extend_from_slice(&rgba[(y * width + x) * 4..][..4]);
            }
        }
    }
    let mut zlib = vec![0x78, 0x01];
    let mut a = 1u32;
    let mut b = 0u32;
    for (index, block) in raw.chunks(65535).enumerate() {
        zlib.push(u8::from((index + 1) * 65535 >= raw.len()));
        let length = block.len() as u16;
        zlib.extend_from_slice(&length.to_le_bytes());
        zlib.extend_from_slice(&(!length).to_le_bytes());
        zlib.extend_from_slice(block);
        for byte in block {
            a = (a + u32::from(*byte)) % 65521;
            b = (b + a) % 65521;
        }
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut output = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 1]);
    chunk(&mut output, b"IHDR", &ihdr);
    chunk(&mut output, b"IDAT", &zlib);
    chunk(&mut output, b"IEND", &[]);
    output
}

fn load(path: &Path, gate: &mut impl FnMut(&str) -> ProbeResult<()>) -> ProbeResult<Vec<u8>> {
    let mut input = fs::File::open(path)?;
    let mut bytes = Vec::new();
    let mut block = [0u8; QUANTUM];
    let mut reads = 0;
    let mut read_time = Duration::ZERO;
    loop {
        gate("read")?;
        let start = Instant::now();
        let size = input.read(&mut block[..QUANTUM.min(ENCODED_LIMIT + 1 - bytes.len())])?;
        bytes.extend_from_slice(&block[..size]);
        read_time += start.elapsed();
        reads += 1;
        if bytes.len() > ENCODED_LIMIT {
            return Err("encoded limit".into());
        }
        if size == 0 {
            break;
        }
    }
    println!(
        "read bytes={} calls={reads} total_us={}",
        bytes.len(),
        read_time.as_micros()
    );
    gate("header")?;
    let mut limits = image::io::Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(32 * 1024 * 1024);
    let decoder = measure("header", || {
        PngDecoder::with_limits(Cursor::new(&bytes), limits)
    })?;
    if decoder.is_apng() {
        return Err("APNG".into());
    }
    let color = decoder.color_type();
    let channels = match color {
        ColorType::L8 => 1,
        ColorType::La8 => 2,
        ColorType::Rgb8 => 3,
        ColorType::Rgba8 => 4,
        _ => return Err("16-bit/unsupported".into()),
    };
    let (width, height) = decoder.dimensions();
    let (width, height) = (width as usize, height as usize);
    gate("allocate")?;
    let mut rgba = measure("cpu_allocate", || vec![0; width * height * 4]);
    gate("reader")?;
    // Phase 0 deliberately probes this API in pinned image 0.24.9. Upgrading the
    // decoder later must revisit its row/interlace and cancellation behavior.
    #[allow(deprecated)]
    let mut reader = measure("reader", || decoder.into_reader())?;
    let rows = (QUANTUM / (width * 4)).max(1);
    let mut native = vec![0; rows * width * channels];
    let mut bands = 0;
    let mut decode_us = 0;
    let mut decode_max_us = 0;
    let mut convert_us = 0;
    for y in (0..height).step_by(rows) {
        gate("decode")?;
        let count = rows.min(height - y) * width;
        let start = Instant::now();
        reader.read_exact(&mut native[..count * channels])?;
        let elapsed = start.elapsed().as_micros();
        decode_us += elapsed;
        decode_max_us = decode_max_us.max(elapsed);
        let start = Instant::now();
        for (source, dest) in native[..count * channels]
            .chunks_exact(channels)
            .zip(rgba[y * width * 4..][..count * 4].chunks_exact_mut(4))
        {
            match channels {
                1 => dest.copy_from_slice(&[source[0], source[0], source[0], 255]),
                2 => dest.copy_from_slice(&[source[0], source[0], source[0], source[1]]),
                3 => dest.copy_from_slice(&[source[0], source[1], source[2], 255]),
                4 => dest.copy_from_slice(source),
                _ => unreachable!(),
            }
        }
        convert_us += start.elapsed().as_micros();
        bands += 1;
    }
    gate("finish")?;
    if reader.read(&mut [0])? != 0 {
        return Err("excess pixels".into());
    }
    println!(
        "decode width={width} height={height} color={color:?} bands={bands} total_us={decode_us} max_band_us={decode_max_us} conversion_us={convert_us}"
    );
    Ok(rgba)
}

// Name every refusal in the receipt. An unlabelled assert leaves the reader unable
// to tell which control produced the surrounding read/header lines, or whether a
// control stopped firing at all.
fn refused(path: &Path, label: &str) -> ProbeResult<()> {
    println!("fixture={label} bytes={}", fs::metadata(path)?.len());
    match load(path, &mut |_| Ok(())) {
        Err(error) => {
            println!("refused fixture={label} error={error}");
            Ok(())
        }
        Ok(_) => Err(format!("fixture {label} was accepted; expected a refusal").into()),
    }
}

fn cpu(root: &Path) -> ProbeResult<()> {
    fs::create_dir_all(root)?;
    let large = pixels(2048, 2048);
    let png = encoded(&large, 2048, 2048);
    assert!(png.len() > 16 * 1024 * 1024 && png.len() < ENCODED_LIMIT);
    assert!(png.len() * 2 < 34 * 1024 * 1024);
    fs::write(root.join("large.png"), png)?;
    fs::write(root.join("interlaced.png"), interlaced(&large, 2048, 2048))?;
    for name in ["large.png", "interlaced.png"] {
        println!("fixture={name}");
        assert_eq!(load(&root.join(name), &mut |_| Ok(()))?, large);
    }
    println!("fixture=kenney");
    let kenney = Path::new("examples/kenney_1-bit-pack_transparent-packed.png");
    let decoded = load(kenney, &mut |_| Ok(()))?;
    assert_eq!(decoded.len(), 784 * 352 * 4);
    assert_eq!(decoded, image::open(kenney)?.into_rgba8().into_raw());

    // Reject malformed boundaries without relying on the production bindings.
    let mut huge = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&u32::MAX.to_be_bytes());
    header.extend_from_slice(&1u32.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut huge, b"IHDR", &header);
    fs::write(root.join("huge.png"), huge)?;
    refused(&root.join("huge.png"), "huge")?;
    fs::write(
        root.join("truncated.png"),
        &fs::read(root.join("large.png"))?[..100],
    )?;
    refused(&root.join("truncated.png"), "truncated")?;

    // The encoded ceiling belongs to the read loop, not the decoder. Padding after
    // IEND keeps the pixels valid, so exactly the cap must still decode while one
    // further byte is refused by the overflow probe before any header parsing.
    println!("fixture=at-limit");
    let mut padded = fs::read(root.join("large.png"))?;
    padded.resize(ENCODED_LIMIT, 0);
    fs::write(root.join("at-limit.png"), &padded)?;
    assert_eq!(load(&root.join("at-limit.png"), &mut |_| Ok(()))?, large);
    padded.push(0);
    fs::write(root.join("over-limit.png"), &padded)?;
    refused(&root.join("over-limit.png"), "over-limit")?;
    for name in ["gray", "gray-alpha", "rgb", "palette", "text", "icc"] {
        println!("fixture={name}");
        assert_eq!(
            load(&root.join(format!("{name}.png")), &mut |_| Ok(()))?,
            fs::read(root.join(format!("{name}.rgba")))?
        );
    }
    for name in ["16bit", "apng"] {
        refused(&root.join(format!("{name}.png")), name)?;
    }

    // Gate every stage from a single bounded mailbox. A blocked native decode
    // stage stays on the worker; cancellation never publishes its partial output.
    for cancel in [false, true] {
        let (grant_tx, grant_rx) = mpsc::sync_channel::<()>(1);
        let (event_tx, event_rx) = mpsc::sync_channel::<&'static str>(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancelled);
        let path = root.join("interlaced.png");
        let worker = std::thread::spawn(move || {
            load(&path, &mut |stage| {
                event_tx.send(if stage == "reader" { "reader" } else { "stage" })?;
                grant_rx.recv()?;
                if worker_cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
                if stage == "reader" {
                    std::thread::sleep(Duration::from_millis(40));
                }
                Ok(())
            })
        });
        let mut heartbeats = 0;
        let mut delayed = false;
        while !worker.is_finished() {
            if let Ok(stage) = event_rx.try_recv() {
                if stage == "reader" {
                    delayed = true;
                }
                grant_tx.send(())?;
            }
            if delayed {
                heartbeats += 1;
                if cancel && heartbeats == 10 {
                    cancelled.store(true, Ordering::Relaxed);
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let result = worker.join().unwrap();
        if cancel {
            assert!(result.is_err());
        } else {
            assert_eq!(result?, large);
        }
        assert!(heartbeats >= 10, "main-thread heartbeat stalled");
        println!(
            "worker cancel={cancel} heartbeat_polls={heartbeats} publication={}",
            !cancel
        );
    }
    println!("CPU PASS");
    Ok(())
}

fn resize(texture: &Texture2D, width: u32, height: u32, pixels: Option<&[u8]>) {
    assert!((1..=2048).contains(&width) && (1..=2048).contains(&height));
    if let Some(bytes) = pixels {
        assert_eq!(bytes.len(), (width * height * 4) as usize);
    }
    let id = texture.raw_miniquad_id();
    // SAFETY: This synchronous probe runs inside the context future. No backend
    // references escape, and no Macroquad/VM/await calls occur inside the borrow.
    unsafe {
        macroquad::window::get_internal_gl()
            .quad_context
            .texture_resize(id, width, height, pixels);
    }
    texture.set_filter(FilterMode::Nearest);
}

fn upload(texture: &Texture2D, width: usize, height: usize, pixels: &[u8]) {
    assert_eq!(pixels.len(), width * height * 4);
    assert_eq!(
        (texture.width() as usize, texture.height() as usize),
        (width, height)
    );
    let id = texture.raw_miniquad_id();
    let rows = (QUANTUM / (width * 4)).max(1);
    let mut maximum = 0;
    let start_all = Instant::now();
    for y in (0..height).step_by(rows) {
        let count = rows.min(height - y);
        let start = Instant::now();
        // SAFETY: Same exclusive context ownership as resize; validated regions
        // and slices. The texture is not published until the final band finishes.
        unsafe {
            macroquad::window::get_internal_gl()
                .quad_context
                .texture_update_part(
                    id,
                    0,
                    y as i32,
                    width as i32,
                    count as i32,
                    &pixels[y * width * 4..][..count * width * 4],
                );
        }
        maximum = maximum.max(start.elapsed().as_micros());
    }
    if height >= 352 {
        println!(
            "upload width={width} height={height} bands={} total_us={} max_band_us={maximum}",
            height.div_ceil(rows),
            start_all.elapsed().as_micros()
        );
    }
}

async fn sliced_upload(texture: &Texture2D, width: usize, height: usize, pixels: &[u8]) {
    let id = texture.raw_miniquad_id();
    let rows = (QUANTUM / (width * 4)).max(1);
    let mut y = 0;
    let mut passes = 0;
    let mut max_pass_us = 0;
    while y < height {
        let start = Instant::now();
        let mut bytes_this_pass = 0;
        for _ in 0..8 {
            if y == height {
                break;
            }
            let count = rows.min(height - y);
            let band = &pixels[y * width * 4..][..count * width * 4];
            // SAFETY: Live, exclusively accessed backend; validated full row band.
            unsafe {
                macroquad::window::get_internal_gl()
                    .quad_context
                    .texture_update_part(id, 0, y as i32, width as i32, count as i32, band);
            }
            y += count;
            bytes_this_pass += band.len();
        }
        assert!(bytes_this_pass <= 256 * 1024);
        max_pass_us = max_pass_us.max(start.elapsed().as_micros());
        passes += 1;
        clear_background(BLACK);
        draw_rectangle(0., 0., passes as f32, 2., WHITE);
        next_frame().await;
    }
    assert_eq!(passes, height.div_ceil(rows * 8));
    assert!(passes > 1);
    assert_eq!(texture.get_texture_data().bytes, pixels);
    println!(
        "sliced_upload passes={passes} max_pass_us={max_pass_us} cap_bytes=262144 exact_final_pixels=true"
    );
}

fn pixel(image: &image::RgbaImage, x: u32, y: u32, expected: [u8; 4]) {
    assert_eq!(image.get_pixel(x, y).0, expected, "pixel at {x},{y}");
}

async fn gpu(root: PathBuf, mode: String) {
    fs::create_dir_all(&root).unwrap();
    // SAFETY: Only query owned backend metadata within the live context.
    println!("backend={:?}", unsafe {
        macroquad::window::get_internal_gl().quad_context.info()
    });
    let mut pool = Vec::new();
    let mut registrations = 0usize;
    for _ in 0..128 {
        pool.push(Texture2D::from_rgba8(1, 1, &[0; 4]));
        registrations += 1;
    }
    let identities: Vec<_> = pool.iter().map(Texture2D::raw_miniquad_id).collect();
    if mode == "gpu-recreate" {
        pool[0] = Texture2D::from_rgba8(1, 1, &[0; 4]);
        registrations += 1;
    }
    assert!(
        registrations <= 128,
        "negative control: texture registration cap"
    );
    let large = pixels(2048, 2048);
    measure("gpu_allocate_2048", || resize(&pool[0], 2048, 2048, None));
    upload(&pool[0], 2048, 2048, &large);
    assert_eq!(pool[0].get_texture_data().bytes, large);
    // Measure actual bounded passes across frame boundaries too, not just region
    // calls made back-to-back. Only a progress bar is drawn until publication.
    resize(&pool[0], 2048, 2048, None);
    sliced_upload(&pool[0], 2048, 2048, &large).await;
    let kenney = image::open("examples/kenney_1-bit-pack_transparent-packed.png")
        .unwrap()
        .into_rgba8();
    measure("gpu_allocate_kenney", || resize(&pool[0], 784, 352, None));
    upload(&pool[0], 784, 352, kenney.as_raw());
    assert_eq!(pool[0].get_texture_data().bytes, *kenney.as_raw());
    for session in 0..100 {
        let count = [0, 1, 128][session % 3];
        for (index, texture) in pool.iter().take(count).enumerate() {
            let (width, height) = (2 + index % 5, 3 + session % 5);
            let data = [session as u8, index as u8, 201, 255].repeat(width * height);
            resize(texture, width as u32, height as u32, None);
            upload(texture, width, height, &data);
            assert_eq!(texture.get_texture_data().bytes, data);
        }
        for (index, texture) in pool.iter().enumerate() {
            resize(texture, 1, 1, Some(&[0; 4]));
            assert_eq!(texture.raw_miniquad_id(), identities[index]);
            assert_eq!(texture.size(), vec2(1., 1.));
            assert_eq!(texture.get_texture_data().bytes, [0; 4]);
        }
        next_frame().await;
    }
    println!("pool sessions=100 registrations={registrations} retired_payload_bytes=512");

    // Asymmetric source surrounded by magenta guard texels. Four source pixels:
    // red, green / blue, white. Expectations below specify the screen directly.
    let mut atlas = [255, 0, 255, 255].repeat(4 * 4);
    for (x, y, color) in [
        (1, 1, [255, 0, 0, 255]),
        (2, 1, [0, 255, 0, 255]),
        (1, 2, [0, 0, 255, 255]),
        (2, 2, [255; 4]),
    ] {
        atlas[(y * 4 + x) * 4..][..4].copy_from_slice(&color);
    }
    resize(&pool[0], 4, 4, None);
    upload(&pool[0], 4, 4, &atlas);
    let mut previous = None;
    for frame in 0..2 {
        clear_background(BLACK);
        for (index, (flip_x, flip_y)) in
            [(false, false), (true, false), (false, true), (true, true)]
                .into_iter()
                .enumerate()
        {
            draw_texture_ex(
                &pool[0],
                8. + index as f32 * 40.,
                8.,
                WHITE,
                DrawTextureParams {
                    source: Some(Rect::new(1., 1., 2., 2.)),
                    dest_size: Some(vec2(32., 32.)),
                    flip_x,
                    flip_y,
                    ..Default::default()
                },
            );
        }
        draw_rectangle(8., 48., 32., 32., Color::from_rgba(0, 0, 255, 255));
        draw_texture_ex(
            &pool[0],
            8.,
            48.,
            Color::new(1., 1., 1., 0.5),
            DrawTextureParams {
                source: Some(Rect::new(1., 1., 1., 1.)),
                dest_size: Some(vec2(32., 32.)),
                ..Default::default()
            },
        );
        if mode == "gpu-early-retire" {
            // Deliberately violate the flush/readback lifetime for the control.
            resize(&pool[0], 1, 1, Some(&[0; 4]));
        }
        let screenshot = get_screen_data();
        let path = root.join(format!("gpu-{frame}.png"));
        capture::save_png(screenshot, &path).unwrap();
        let image = image::open(&path).unwrap().into_rgba8();
        for (index, expected) in [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255; 4],
        ]
        .into_iter()
        .enumerate()
        {
            pixel(&image, 12 + index as u32 * 40, 12, expected);
        }
        pixel(&image, 9, 9, [255, 0, 0, 255]);
        pixel(&image, 39, 39, [255; 4]);
        pixel(&image, 7, 9, [0, 0, 0, 255]);
        let blend = image.get_pixel(12, 52).0;
        assert!((i16::from(blend[0]) - 128).abs() <= 1 && (i16::from(blend[2]) - 127).abs() <= 1);
        if let Some(bytes) = &previous {
            assert_eq!(&fs::read(&path).unwrap(), bytes);
        }
        previous = Some(fs::read(path).unwrap());
        next_frame().await;
    }
    // Simulate CPU/session destruction after queueing the final draw. Only the
    // GPU owner survives until readback, exactly as required by Player shutdown.
    draw_texture_ex(
        &pool[0],
        8.,
        8.,
        WHITE,
        DrawTextureParams {
            source: Some(Rect::new(1., 1., 1., 1.)),
            dest_size: Some(vec2(32., 32.)),
            ..Default::default()
        },
    );
    drop(atlas);
    let screenshot = get_screen_data();
    capture::save_png(screenshot, &root.join("gpu-shutdown.png")).unwrap();
    pixel(
        &image::open(root.join("gpu-shutdown.png"))
            .unwrap()
            .into_rgba8(),
        12,
        12,
        [255, 0, 0, 255],
    );
    for texture in &pool {
        resize(texture, 1, 1, Some(&[0; 4]));
    }
    drop(pool); // Still inside the graphics-context future.
    println!("GPU PASS mode={mode}");
}

fn main() {
    let mode = std::env::args()
        .nth(1)
        .expect("cpu | gpu | gpu-recreate | gpu-early-retire");
    let root = PathBuf::from("target/png-sprite-probe");
    if mode == "cpu" {
        cpu(&root).unwrap();
        return;
    }
    assert!(["gpu", "gpu-recreate", "gpu-early-retire"].contains(&mode.as_str()));
    macroquad::Window::from_config(
        Conf {
            window_title: "Protogine PNG Phase 0 probe".into(),
            window_width: 176,
            window_height: 96,
            window_resizable: false,
            high_dpi: false,
            ..Default::default()
        },
        gpu(root.join(&mode), mode),
    );
}
