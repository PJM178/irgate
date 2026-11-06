pub mod capture;
pub mod config;
pub mod detector;
pub mod error;
pub mod inference;
pub mod preprocess;
pub mod storage;
pub mod user;
pub mod verify;

pub use crate::capture::Camera;
pub use crate::config::FaceAuthConfig;
use crate::detector::{assess_frame, Face, FaceDetector, FrameQuality};
use crate::error::FaceAuthError;
use crate::inference::FaceEncoder;
use crate::storage::EmbeddingStore;
use crate::verify::{best_similarity, verify_embedding};
use anyhow::Result;
use std::time::{Duration, Instant};

/// Progress reporting for the interactive enrolment paths.
///
/// The library never writes to stdout itself — `face-auth` runs under
/// `pam_exec`, where stray output lands on the user's terminal on every
/// `sudo`. Callers that *are* interactive supply a sink.
pub type ProgressFn<'a> = &'a mut dyn FnMut(EnrollProgress);

#[derive(Debug, Clone)]
pub enum EnrollProgress {
    Capturing { captured: usize, wanted: usize, attempt: usize },
    NoContent,
    NoFace,
    /// The frame was fine but near-identical to one already captured this
    /// run; the user should change pose a little.
    TooSimilar,
    Captured { captured: usize, wanted: usize },
}

/// Frames whose embedding is at least this similar to one already captured in
/// the same enrolment run are skipped. Five frames of one held pose are one
/// template five times over; what makes matching tolerant of a turned head or
/// a different distance is having *seen* those, so enrolment nudges the user
/// to move. Consecutive still frames measure 0.98-0.99 on the reference
/// camera; a small turn drops to the low 0.9s.
pub const ENROLL_DUPLICATE_SIMILARITY: f32 = 0.97;

/// Default number of templates an enrolment captures.
pub const ENROLL_DEFAULT_FRAMES: usize = 8;
/// Default pause between enrolment captures, in milliseconds. Long enough
/// for the user to move between frames.
pub const ENROLL_DEFAULT_INTERVAL_MS: u64 = 500;

/// What a scan window produced. Returned rather than a bare `bool` so the
/// caller can log *why*: "no face passed the quality gate" and "a face scored
/// 0.55 against a 0.60 threshold" need very different fixes, and neither is
/// visible from an exit status.
#[derive(Debug, Clone)]
pub struct ScanOutcome {
    pub matched: bool,
    /// Highest similarity over every frame that reached the encoder; `None`
    /// if no frame did.
    pub best_similarity: Option<f32>,
    pub threshold: f32,
    /// Frames captured during the window.
    pub frames: usize,
    /// Frames in which a face was found and encoded.
    pub faces: usize,
    /// Why the most recent unusable frame was rejected before detection.
    pub last_reject: Option<FrameQuality>,
    pub elapsed: Duration,
}

impl ScanOutcome {
    /// One-line summary for logs: outcome, score, and what the scan saw.
    pub fn summary(&self) -> String {
        let score = match self.best_similarity {
            Some(s) => format!("{s:.3}"),
            None => "none".to_string(),
        };
        let mut s = format!(
            "{} similarity={score} threshold={:.2} frames={} faces={} elapsed={}ms",
            if self.matched { "match" } else { "no match" },
            self.threshold,
            self.frames,
            self.faces,
            self.elapsed.as_millis()
        );
        if self.faces == 0 {
            match &self.last_reject {
                Some(q) => s.push_str(&format!(" last_reject=\"{q}\"")),
                None if self.frames > 0 => s.push_str(" (frames passed quality checks but no face was detected)"),
                None => {}
            }
        }
        s
    }
}

pub struct FaceAuth {
    config: FaceAuthConfig,
    encoder: FaceEncoder,
    detector: FaceDetector,
}

impl FaceAuth {
    pub fn new(config: FaceAuthConfig) -> Result<Self> {
        config.validate()?;
        let encoder = FaceEncoder::new(&config.model_path())?;
        let detector = FaceDetector::new(&config.detector_model_path(), config.detector_threshold())?;
        Ok(Self { config, encoder, detector })
    }

    pub fn config(&self) -> &FaceAuthConfig {
        &self.config
    }

    /// Detect the face in an equalised frame and encode it.
    ///
    /// `Ok(None)` means no face reached the threshold. The crop fed to the
    /// encoder is the landmark-aligned 112x112 face, never the frame itself:
    /// embedding the whole frame made the template describe the room, and an
    /// empty chair scored above the match threshold.
    pub fn probe_frame(&mut self, frame: &crate::capture::IrFrame) -> Result<Option<(Face, Vec<f32>)>> {
        self.encode_face(frame)
    }

    fn encode_face(&mut self, frame: &crate::capture::IrFrame) -> Result<Option<(Face, Vec<f32>)>> {
        let Some(face) = self.detector.detect(frame)? else {
            return Ok(None);
        };
        let input = crate::preprocess::align_face(frame, &face.landmarks)?;
        let embedding = self.encoder.encode(input.view())?;
        Ok(Some((face, embedding)))
    }

    /// Single-shot verification. Used by the settings GUI's test button; the
    /// PAM path uses [`FaceAuth::authenticate_scan`].
    pub fn authenticate_once(&mut self, user: &str) -> Result<bool> {
        let t0 = Instant::now();
        let store = EmbeddingStore::load(user, &self.config.embeddings_dir())?;
        tracing::debug!(elapsed = ?t0.elapsed(), "store loaded");

        let t1 = Instant::now();
        let frame = crate::capture::capture_ir_frame(
            &self.config.device()?,
            self.config.capture_timeout_ms(),
        )?;
        tracing::debug!(elapsed = ?t1.elapsed(), "frame captured");

        let quality = assess_frame(&frame);
        if quality != FrameQuality::Ok {
            tracing::debug!(%quality, "frame rejected before inference");
            return Err(FaceAuthError::NoFaceDetected.into());
        }

        let mut frame = frame;
        crate::preprocess::histogram_equalize(&mut frame);

        let Some((_, embedding)) = self.encode_face(&frame)? else {
            return Err(FaceAuthError::NoFaceDetected.into());
        };
        tracing::debug!(elapsed = ?t0.elapsed(), "authenticate_once complete");

        verify_embedding(&embedding, &store, self.config.threshold())
    }

    /// Keep capturing until a frame matches or the scan window closes.
    pub fn authenticate_scan(
        &mut self,
        user: &str,
        duration_ms: u64,
        interval_ms: u64,
    ) -> Result<ScanOutcome> {
        let t0 = Instant::now();
        let threshold = self.config.threshold();
        anyhow::ensure!(
            threshold.is_finite() && threshold > 0.0,
            "refusing to scan against non-positive threshold {threshold}"
        );
        let store = EmbeddingStore::load(user, &self.config.embeddings_dir())?;
        tracing::debug!(elapsed = ?t0.elapsed(), "store loaded");

        let mut cam = Camera::open(&self.config.device()?)?;
        tracing::debug!(elapsed = ?t0.elapsed(), "camera open");

        let deadline = Instant::now() + Duration::from_millis(duration_ms);
        let mut frame_num: usize = 0;
        let mut faces: usize = 0;
        let mut best: Option<f32> = None;
        let mut consecutive_errors = 0u32;
        let mut last_reject: Option<FrameQuality> = None;

        // Wait out the remainder of the interval without overrunning the window.
        let nap = |deadline: Instant| {
            let sleep =
                Duration::from_millis(interval_ms).min(deadline.saturating_duration_since(Instant::now()));
            if !sleep.is_zero() {
                std::thread::sleep(sleep);
            }
        };

        loop {
            if Instant::now() >= deadline {
                // If nothing ever reached the detector, say why: "no match" and
                // "the illuminator never fired" need very different fixes.
                match last_reject {
                    Some(q) => tracing::debug!(
                        frames = frame_num,
                        "scan window elapsed; no frame passed quality checks — last: {q}"
                    ),
                    None => tracing::debug!(frames = frame_num, "scan window elapsed without a match"),
                }
                return Ok(ScanOutcome {
                    matched: false,
                    best_similarity: best,
                    threshold,
                    frames: frame_num,
                    faces,
                    last_reject,
                    elapsed: t0.elapsed(),
                });
            }

            frame_num += 1;
            let frame = match cam.capture_illuminated_frame(self.config.capture_timeout_ms()) {
                Ok(f) => {
                    consecutive_errors = 0;
                    f
                }
                Err(e) => {
                    consecutive_errors += 1;
                    tracing::warn!(frame = frame_num, error = %e, "capture failed");
                    if consecutive_errors >= 3 {
                        return Err(e);
                    }
                    nap(deadline);
                    continue;
                }
            };

            let quality = assess_frame(&frame);
            if quality != FrameQuality::Ok {
                tracing::trace!(frame = frame_num, %quality, "frame rejected");
                last_reject = Some(quality);
                nap(deadline);
                continue;
            }

            let mut frame = frame;
            crate::preprocess::histogram_equalize(&mut frame);

            let Some((face, embedding)) = self.encode_face(&frame)? else {
                nap(deadline);
                continue;
            };
            faces += 1;
            tracing::trace!(
                frame = frame_num,
                score = face.score,
                width = face.width(),
                "face detected"
            );

            let similarity = best_similarity(&embedding, &store)?;
            tracing::debug!(frame = frame_num, similarity, threshold, "frame scored");
            if best.map_or(true, |b| similarity > b) {
                best = Some(similarity);
            }

            if similarity >= threshold {
                tracing::debug!(frame = frame_num, elapsed = ?t0.elapsed(), "match");
                return Ok(ScanOutcome {
                    matched: true,
                    best_similarity: best,
                    threshold,
                    frames: frame_num,
                    faces,
                    last_reject,
                    elapsed: t0.elapsed(),
                });
            }

            nap(deadline);
        }
    }

    fn capture_embeddings(
        &mut self,
        cam: &mut Camera,
        store: &mut EmbeddingStore,
        frames: usize,
        interval_ms: u64,
        progress: ProgressFn<'_>,
    ) -> Result<()> {
        let mut captured = 0usize;
        let mut attempts = 0usize;
        let max_attempts = frames.saturating_mul(3);
        let before = store.embeddings.len();
        let mut last_reject: Option<FrameQuality> = None;

        while captured < frames && attempts < max_attempts {
            attempts += 1;
            progress(EnrollProgress::Capturing { captured, wanted: frames, attempt: attempts });

            let frame = cam.capture_illuminated_frame(self.config.capture_timeout_ms())?;

            let quality = assess_frame(&frame);
            if quality != FrameQuality::Ok {
                last_reject = Some(quality);
                progress(EnrollProgress::NoContent);
                std::thread::sleep(Duration::from_millis(interval_ms));
                continue;
            }

            let mut frame = frame;
            crate::preprocess::histogram_equalize(&mut frame);

            let Some((_, embedding)) = self.encode_face(&frame)? else {
                progress(EnrollProgress::NoFace);
                std::thread::sleep(Duration::from_millis(interval_ms));
                continue;
            };

            // Only frames from *this* run count as duplicates: --improve is
            // meant to add poses the existing templates lack, and a frame that
            // resembles an old template is exactly the kind worth refreshing.
            let duplicate = store.embeddings[before..].iter().any(|e| {
                e.iter().zip(&embedding).map(|(a, b)| a * b).sum::<f32>()
                    >= ENROLL_DUPLICATE_SIMILARITY
            });
            if duplicate {
                progress(EnrollProgress::TooSimilar);
                std::thread::sleep(Duration::from_millis(interval_ms));
                continue;
            }

            store.add_embedding(embedding);
            captured += 1;
            progress(EnrollProgress::Captured { captured, wanted: frames });

            if captured < frames {
                std::thread::sleep(Duration::from_millis(interval_ms));
            }
        }

        // Check what *this* run produced. Testing the whole store would let an
        // append silently succeed having captured nothing.
        if store.embeddings.len() == before {
            match last_reject {
                Some(q) => anyhow::bail!(
                    "no usable frame in {} attempts: {}\n\
                     Run `cargo run --example frame-stats` to see what the sensor is \
                     delivering.",
                    attempts,
                    q
                ),
                None => anyhow::bail!(
                    "no face detected in any of {} attempts — check the camera is the IR \
                     sensor and that your face is lit and in frame",
                    attempts
                ),
            }
        }

        Ok(())
    }

    /// Replace the user's enrolled embeddings.
    pub fn enroll(
        &mut self,
        user: &str,
        frames: usize,
        interval_ms: u64,
        progress: ProgressFn<'_>,
    ) -> Result<usize> {
        let mut store = EmbeddingStore::default();
        let mut cam = Camera::open(&self.config.device()?)?;
        self.capture_embeddings(&mut cam, &mut store, frames, interval_ms, progress)?;

        let saved = store.embeddings.len();
        store.save(user, &self.config.embeddings_dir())?;
        Ok(saved)
    }

    /// Append to the user's enrolled embeddings, improving coverage across
    /// lighting and angles.
    pub fn enroll_append(
        &mut self,
        user: &str,
        frames: usize,
        interval_ms: u64,
        progress: ProgressFn<'_>,
    ) -> Result<(usize, usize)> {
        let mut store = match EmbeddingStore::load(user, &self.config.embeddings_dir()) {
            Ok(s) => s,
            // Only "nothing enrolled yet" starts from empty. Any other error
            // (corrupt or unreadable file) must not silently discard what is
            // already there — saving would overwrite it.
            Err(e) if matches!(e.downcast_ref::<FaceAuthError>(), Some(FaceAuthError::NoEmbeddings)) => {
                EmbeddingStore::default()
            }
            Err(e) => return Err(e.context("refusing to append: existing embeddings unreadable")),
        };

        let existing = store.embeddings.len();
        let mut cam = Camera::open(&self.config.device()?)?;
        self.capture_embeddings(&mut cam, &mut store, frames, interval_ms, progress)?;

        let total = store.embeddings.len();
        store.save(user, &self.config.embeddings_dir())?;
        Ok((total - existing, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_sane() {
        let config = FaceAuthConfig::default();
        assert_eq!(config.threshold(), crate::config::DEFAULT_THRESHOLD);
        assert!(config.validate().is_ok());
    }
}
