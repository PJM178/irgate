//! Diagnostic: does the stream survive a dark room?
//!
//! In darkness the reference sensor blanks to a flat fill about 0.8 s after
//! each stream start; `Camera::capture_frame` restarts the stream when that
//! happens. This streams for N seconds (default 10) through that path and
//! reports usable lit frames per second, restarts, and how long a restart
//! costs, which is the gap between the last frame before it and the first after.
//!
//!   cargo run --release --example darkstream -- [SECONDS]
use face_auth_core::capture::Camera;
use face_auth_core::FaceAuthConfig;
use std::time::{Duration, Instant};

/// Lit strobe frames average 30–100 of 255 on the reference sensor, unlit 0–8.
const LIT_MEAN: f64 = 20.0 * 257.0;
/// Means above this are blown out: most of the face is saturated.
const OVEREXPOSED_MEAN: f64 = 160.0 * 257.0;

fn main() -> anyhow::Result<()> {
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let config = FaceAuthConfig::load()?;
    let device = config.device()?;
    println!("device: {device}, streaming {seconds} s\n");

    let mut cam = Camera::open(&device)?;
    let start = Instant::now();
    let end = start + Duration::from_secs(seconds);

    let (mut frames, mut lit, mut overexposed) = (0u32, 0u32, 0u32);
    let mut lit_per_sec = vec![0u32; seconds as usize + 1];
    let mut gaps: Vec<Duration> = Vec::new();
    let mut last = Instant::now();
    // Lit-frame means (0-255) of the current segment between restarts.
    let mut segment: Vec<u32> = Vec::new();

    while Instant::now() < end {
        let before = cam.stream_restarts();
        let frame = cam.capture_frame(config.capture_timeout_ms())?;
        let now = Instant::now();
        if cam.stream_restarts() > before {
            gaps.push(now - last);
            println!(
                "{:6.2} s  restart #{} (gap {:.0?})  lit means before it: {:?}",
                (now - start).as_secs_f64(),
                cam.stream_restarts(),
                now - last,
                segment
            );
            segment.clear();
        }
        last = now;
        frames += 1;

        let mean = frame.mean_intensity();
        if mean >= LIT_MEAN {
            lit += 1;
            segment.push((mean / 257.0).round() as u32);
            lit_per_sec[(now - start).as_secs() as usize] += 1;
            if mean > OVEREXPOSED_MEAN {
                overexposed += 1;
            }
        }
    }

    println!("lit means after last restart: {segment:?}");
    let elapsed = start.elapsed().as_secs_f64();
    println!("\nframes {frames}, lit {lit} ({:.1}/s), of which overexposed {overexposed}", lit as f64 / elapsed);
    println!("restarts {}", cam.stream_restarts());
    if !gaps.is_empty() {
        let mean_gap = gaps.iter().sum::<Duration>() / gaps.len() as u32;
        println!("restart gap: mean {mean_gap:.0?}, max {:.0?}", gaps.iter().max().unwrap());
    }
    println!("lit frames per second: {:?}", &lit_per_sec[..seconds as usize]);
    Ok(())
}
