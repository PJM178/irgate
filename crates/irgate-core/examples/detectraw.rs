//! Diagnostic: run the quality gate and face detector on raw GREY frames
//! captured with `v4l2-ctl --stream-to=FILE` (640x400 by default).
//!
//!   cargo run --release --example detectraw -- FILE [WIDTH HEIGHT]
//!
//! Prints per frame: mean, quality verdict, and the detector's best face
//! (score, box size) before and after histogram equalisation.
use irgate_core::capture::IrFrame;
use irgate_core::detector::{assess_frame, FaceDetector};
use irgate_core::FaceAuthConfig;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).ok_or_else(|| anyhow::anyhow!("usage: detectraw FILE [W H]"))?;
    let w: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(640);
    let h: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(400);

    let config = FaceAuthConfig::load()?;
    // Threshold 0 so near-misses show their score too.
    let mut detector = FaceDetector::new(&config.detector_model_path(), 0.0)?;
    println!("configured detector threshold: {}", config.detector_threshold());

    let raw = std::fs::read(path)?;
    for (i, chunk) in raw.chunks_exact(w * h).enumerate() {
        let frame = IrFrame {
            data: chunk.iter().map(|&b| b as u16 * 257).collect(),
            width: w as u32,
            height: h as u32,
        };
        let mean = chunk.iter().map(|&b| b as f64).sum::<f64>() / chunk.len() as f64;
        let quality = assess_frame(&frame);
        let describe = |f: Option<irgate_core::detector::Face>| match f {
            Some(f) => format!("score {:.2} box {:.0}x{:.0}", f.score, f.width(), f.height()),
            None => "none".into(),
        };
        let plain = describe(detector.detect(&frame)?);
        let mut eq = frame;
        irgate_core::preprocess::histogram_equalize(&mut eq);
        let equalised = describe(detector.detect(&eq)?);
        println!("{i:3} mean {mean:5.1} {quality:<12} raw: {plain:<26} equalised: {equalised}");
    }
    Ok(())
}
