//! Diagnostic: how well do enrolled templates hold up, and what does the
//! pipeline see when nobody is there?
//!
//! Runs the real library pipeline (quality gate, CLAHE, SCRFD detection,
//! landmark alignment, encoder) against templates in a directory you own, so
//! it needs no root and never touches the system store. Honours the usual
//! config layering, so `FACE_AUTH_DETECTOR_MODEL_PATH=...` works.
//!
//!   probe enroll <dir>          enrol into <dir> (move your head while it runs)
//!   probe check  <dir> <secs>   score live frames against <dir> for <secs>
//!   probe scene  <dir> <secs>   same, reporting how many frames had a face
//!                               at all — run it with the chair empty
use face_auth_core::detector::{assess_frame, FrameQuality};
use face_auth_core::preprocess::histogram_equalize;
use face_auth_core::storage::EmbeddingStore;
use face_auth_core::verify::best_similarity;
use face_auth_core::{
    Camera, EnrollProgress, FaceAuth, FaceAuthConfig, ENROLL_DEFAULT_FRAMES,
    ENROLL_DEFAULT_INTERVAL_MS,
};
use std::time::{Duration, Instant};

const USER: &str = "probe";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (mode, dir) = match (args.get(1), args.get(2)) {
        (Some(m), Some(d)) => (m.as_str(), d.clone()),
        _ => anyhow::bail!("usage: probe enroll <dir> | probe check <dir> <secs> | probe scene <dir> <secs>"),
    };
    let secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(20);

    let mut config = FaceAuthConfig::load()?;
    config.embeddings_dir = Some(dir.clone());
    config.validate()?;
    let threshold = config.threshold();
    let mut auth = FaceAuth::new(config)?;

    match mode {
        "enroll" => {
            println!("Enrolling into {dir}. Slowly turn and tilt your head while this runs.");
            let mut progress = |p: EnrollProgress| match p {
                EnrollProgress::Capturing { captured, wanted, attempt } => {
                    println!("capturing {}/{wanted} (attempt {attempt})", captured + 1)
                }
                EnrollProgress::NoContent => println!("  nothing in frame"),
                EnrollProgress::NoFace => println!("  no face"),
                EnrollProgress::TooSimilar => println!("  too similar to a captured frame — move a little"),
                EnrollProgress::Captured { captured, wanted } => println!("  captured {captured}/{wanted}"),
            };
            let n = auth.enroll(USER, ENROLL_DEFAULT_FRAMES, ENROLL_DEFAULT_INTERVAL_MS, &mut progress)?;
            println!("saved {n} templates");
        }
        "check" | "scene" => {
            let store = EmbeddingStore::load(USER, std::path::Path::new(&dir))?;
            println!("{} templates, threshold {threshold:.2}", store.embeddings.len());
            println!("{:>5} {:>5} {:>6} {:>7} {:>6}", "t", "det", "width", "sim", "");

            let mut cam = Camera::open(&auth.config().device())?;
            let timeout = auth.config().capture_timeout_ms();
            let t0 = Instant::now();
            let (mut frames, mut faces) = (0usize, 0usize);
            let mut sims: Vec<f32> = Vec::new();
            let mut last_print = Instant::now() - Duration::from_secs(1);
            let mut last_reject: Option<FrameQuality> = None;

            while t0.elapsed() < Duration::from_secs(secs) {
                let frame = cam.capture_illuminated_frame(timeout)?;
                frames += 1;
                let q = assess_frame(&frame);
                if q != FrameQuality::Ok {
                    last_reject = Some(q);
                    continue;
                }
                let mut frame = frame;
                histogram_equalize(&mut frame);

                let (face, sim) = match auth.probe_frame(&frame)? {
                    Some((face, emb)) => {
                        faces += 1;
                        let s = best_similarity(&emb, &store)?;
                        sims.push(s);
                        (Some(face), Some(s))
                    }
                    None => (None, None),
                };

                if last_print.elapsed() >= Duration::from_millis(500) {
                    match (face, sim) {
                        (Some(f), Some(s)) => println!(
                            "{:>5.1} {:>5.2} {:>6.0} {:>7.3} {}",
                            t0.elapsed().as_secs_f32(),
                            f.score,
                            f.width(),
                            s,
                            if s >= threshold { "match" } else { "" }
                        ),
                        _ => println!("{:>5.1} {:>5} {:>6} {:>7}  no face", t0.elapsed().as_secs_f32(), "-", "-", "-"),
                    }
                    last_print = Instant::now();
                }
            }

            println!("\nframes {frames}, usable with a face {faces}");
            if let (0, Some(q)) = (faces, last_reject) {
                println!("last rejected frame: {q}");
            }
            if !sims.is_empty() {
                let n = sims.len() as f32;
                let mean = sims.iter().sum::<f32>() / n;
                let min = sims.iter().copied().fold(f32::INFINITY, f32::min);
                let max = sims.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let below = sims.iter().filter(|&&s| s < threshold).count();
                println!(
                    "similarity: mean {mean:.3}  min {min:.3}  max {max:.3}  below {threshold:.2}: {below}/{}",
                    sims.len()
                );
            } else {
                println!("no face was ever encoded, so nothing was scored — which is the point of the scene test");
            }
        }
        _ => anyhow::bail!("unknown mode {mode}"),
    }
    Ok(())
}
