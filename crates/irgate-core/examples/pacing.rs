//! Diagnostic: does the pause between captures change what the detector sees?
//!
//! Enrollment sleeps between attempts (500 ms by default) while the stream
//! keeps running; the scan loop does not. This captures with each interval
//! and prints frame mean, stream restarts, and the detector's best score.
//!
//!   cargo run --release --example pacing -- [ATTEMPTS] [INTERVAL_MS...]
use irgate_core::capture::Camera;
use irgate_core::detector::FaceDetector;
use irgate_core::FaceAuthConfig;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let attempts: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(12);
    let intervals: Vec<u64> = if args.len() > 2 {
        args[2..].iter().filter_map(|s| s.parse().ok()).collect()
    } else {
        vec![0, 500]
    };

    let config = FaceAuthConfig::load()?;
    let mut detector = FaceDetector::new(&config.detector_model_path(), 0.0)?;
    let mut cam = Camera::open(&config.device()?)?;
    println!("detector threshold {}\n", config.detector_threshold());

    for interval in intervals {
        let mut passed = 0;
        let mut scores = Vec::new();
        for _ in 0..attempts {
            let before = cam.stream_restarts();
            let mut frame = cam.capture_illuminated_frame(config.capture_timeout_ms())?;
            let mean = frame.mean_intensity() / 257.0;
            irgate_core::preprocess::histogram_equalize(&mut frame);
            let score = detector.detect(&frame)?.map_or(0.0, |f| f.score);
            if score >= config.detector_threshold() {
                passed += 1;
            }
            scores.push(format!("{mean:.0}/{score:.2}{}", if cam.stream_restarts() > before { "R" } else { "" }));
            if interval > 0 {
                cam.pause(); // as enrollment does between attempts
                std::thread::sleep(Duration::from_millis(interval));
            }
        }
        println!("interval {interval:>4} ms: {passed}/{attempts} detected   (mean/score, R = restart)");
        println!("  {}", scores.join(" "));
    }
    Ok(())
}
