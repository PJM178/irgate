//! Face detection with landmarks: SCRFD (`det_500m.onnx` from InsightFace's
//! `buffalo_sc` pack), the detector the recognition model was built to sit
//! behind.
//!
//! The previous detector answered only "is there a face somewhere in the
//! frame?" and its bounding boxes were discarded, so the recogniser was fed
//! the whole 640x400 frame squashed to 112x112. Measured on the reference
//! machine with that yes/no check bypassed, frames of the empty chair scored
//! 0.65–0.77 against the enrolled template, above the 0.60 threshold: the
//! embedding described the room, not the face. This detector returns the five landmarks the encoder's alignment
//! needs (see `preprocess::align_face`).

use image::DynamicImage;
use tract_onnx::prelude::*;

use crate::capture::IrFrame;

// Frame-quality gates, expressed in 8-bit-equivalent units so the numbers can
// be compared against what `cargo run --example frame-stats` prints.
//
// Measured on the reference ASUS IR sensor:
//   illuminated frames   mean 48-96,  variance  82-317
//   dark (strobe off)    mean 1.8-8,  variance 3.4-39
const MIN_FRAME_MEAN_8BIT: f64 = 12.0;
const MIN_FRAME_VARIANCE_8BIT: f64 = 20.0;

/// Samples are u16 but carry 8-bit data widened by 257 (see `capture_frame`).
const U16_PER_8BIT: f64 = 257.0;

/// Why a frame was rejected before inference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FrameQuality {
    Ok,
    Empty,
    /// Almost certainly an unlit frame from a strobing IR illuminator.
    TooDark { mean_8bit: f64 },
    /// Uniform field — a covered lens, or a wall.
    TooFlat { variance_8bit: f64 },
}

impl std::fmt::Display for FrameQuality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameQuality::Ok => write!(f, "ok"),
            FrameQuality::Empty => write!(f, "empty frame"),
            FrameQuality::TooDark { mean_8bit } => write!(
                f,
                "too dark (mean {mean_8bit:.1}/255, need {MIN_FRAME_MEAN_8BIT}) \
                 — unlit frame, or the IR illuminator is not firing"
            ),
            FrameQuality::TooFlat { variance_8bit } => write!(
                f,
                "too flat (variance {variance_8bit:.1}, need {MIN_FRAME_VARIANCE_8BIT}) \
                 — lens covered, or nothing in view"
            ),
        }
    }
}

/// Square input side. SCRFD is fully convolutional; InsightFace's default is
/// 640, but a 640x400 IR frame letterboxed into 320x320 keeps a face at
/// arm's length around 60px wide, well above the stride-8 anchors, and runs
/// in a quarter of the time (24ms vs 94ms under tract on the reference
/// machine). Raise this if faces are routinely small in frame.
pub const DETECTOR_INPUT: usize = 320;

/// Feature-map strides, in the order the model emits its outputs.
const STRIDES: [usize; 3] = [8, 16, 32];
/// Anchors per feature-map cell; consecutive output rows share a centre.
const ANCHORS_PER_CELL: usize = 2;
/// Outputs per stride group: scores, boxes, landmarks.
const OUTPUT_GROUPS: usize = 3;

/// A detected face, in frame pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Face {
    pub score: f32,
    /// `[x1, y1, x2, y2]`.
    pub bbox: [f32; 4],
    /// Left eye, right eye, nose tip, left mouth corner, right mouth corner —
    /// "left" and "right" as seen in the image.
    pub landmarks: [[f32; 2]; 5],
}

impl Face {
    pub fn width(&self) -> f32 {
        self.bbox[2] - self.bbox[0]
    }

    pub fn height(&self) -> f32 {
        self.bbox[3] - self.bbox[1]
    }
}

pub struct FaceDetector {
    model: std::sync::Arc<TypedRunnableModel>,
    threshold: f32,
}

impl FaceDetector {
    pub fn new(model_path: &str, threshold: f32) -> anyhow::Result<Self> {
        if !std::path::Path::new(model_path).exists() {
            anyhow::bail!("face detector model not found at {model_path}");
        }
        // The input fact is required before optimisation: the model's spatial
        // dimensions are symbolic and tract cannot lower it without them.
        let model = onnx()
            .model_for_path(model_path)?
            .with_input_fact(
                0,
                InferenceFact::dt_shape(
                    f32::datum_type(),
                    tvec!(1, 3, DETECTOR_INPUT, DETECTOR_INPUT),
                ),
            )?
            .into_optimized()?
            .into_runnable()?;
        Ok(Self { model, threshold })
    }

    /// The highest-scoring face at or above the threshold, if any.
    ///
    /// Authentication wants exactly one face. If two are present this picks
    /// the one the detector is surest of, which in practice is the larger and
    /// nearer one; whoever is sitting at the console.
    pub fn detect(&mut self, frame: &IrFrame) -> anyhow::Result<Option<Face>> {
        let (input, scale) = preprocess_for_detector(frame)?;
        let mut input = input.into_dyn();
        input.insert_axis_inplace(tract_ndarray::Axis(0));
        let result = self.model.run(tvec!(Tensor::from(input).into_tvalue()))?;

        anyhow::ensure!(
            result.len() == STRIDES.len() * OUTPUT_GROUPS,
            "unexpected detector output count {}, expected {}",
            result.len(),
            STRIDES.len() * OUTPUT_GROUPS
        );

        let mut best: Option<Face> = None;

        for (i, &stride) in STRIDES.iter().enumerate() {
            let scores = result[i].to_plain_array_view::<f32>()?;
            let boxes = result[i + STRIDES.len()].to_plain_array_view::<f32>()?;
            let kps = result[i + 2 * STRIDES.len()].to_plain_array_view::<f32>()?;

            let cells = DETECTOR_INPUT / stride;
            let rows = cells * cells * ANCHORS_PER_CELL;
            // Check before indexing: a mismatched model would otherwise panic
            // inside PAM.
            anyhow::ensure!(
                scores.shape() == [rows, 1]
                    && boxes.shape() == [rows, 4]
                    && kps.shape() == [rows, 10],
                "unexpected detector output shapes at stride {stride}: {:?} {:?} {:?}",
                scores.shape(),
                boxes.shape(),
                kps.shape()
            );

            for r in 0..rows {
                let score = scores[[r, 0]];
                if !score.is_finite() || score < self.threshold {
                    continue;
                }
                if best.map_or(false, |b| score <= b.score) {
                    continue;
                }

                // Rows come in groups of ANCHORS_PER_CELL per cell, cells in
                // row-major order over the feature map.
                let cell = r / ANCHORS_PER_CELL;
                let cx = ((cell % cells) * stride) as f32;
                let cy = ((cell / cells) * stride) as f32;
                let s = stride as f32;

                // Boxes are distances from the anchor centre to each edge,
                // landmarks are offsets from it, both in units of the stride.
                let bbox = [
                    (cx - boxes[[r, 0]] * s) / scale,
                    (cy - boxes[[r, 1]] * s) / scale,
                    (cx + boxes[[r, 2]] * s) / scale,
                    (cy + boxes[[r, 3]] * s) / scale,
                ];
                let mut landmarks = [[0.0f32; 2]; 5];
                for (k, lm) in landmarks.iter_mut().enumerate() {
                    *lm = [
                        (cx + kps[[r, 2 * k]] * s) / scale,
                        (cy + kps[[r, 2 * k + 1]] * s) / scale,
                    ];
                }
                if bbox.iter().chain(landmarks.iter().flatten()).any(|v| !v.is_finite()) {
                    continue;
                }
                best = Some(Face { score, bbox, landmarks });
            }
        }

        Ok(best)
    }
}

/// Cheap "is this frame worth running inference on" check.
///
/// Accumulates in f64: a 640x400 frame sums ~256k terms of up to 4.3e9, well
/// past what f32's ~7 significant digits can carry.
pub fn assess_frame(frame: &IrFrame) -> FrameQuality {
    if frame.data.is_empty() {
        return FrameQuality::Empty;
    }
    let len = frame.data.len() as f64;
    let mean = frame.data.iter().map(|&v| v as f64).sum::<f64>() / len;
    let variance: f64 = frame
        .data
        .iter()
        .map(|&v| {
            let d = v as f64 - mean;
            d * d
        })
        .sum::<f64>()
        / len;

    let mean_8bit = mean / U16_PER_8BIT;
    let variance_8bit = variance / (U16_PER_8BIT * U16_PER_8BIT);

    if mean_8bit < MIN_FRAME_MEAN_8BIT {
        return FrameQuality::TooDark { mean_8bit };
    }
    if variance_8bit < MIN_FRAME_VARIANCE_8BIT {
        return FrameQuality::TooFlat { variance_8bit };
    }
    FrameQuality::Ok
}

pub fn raw_frame_has_content(frame: &IrFrame) -> bool {
    assess_frame(frame) == FrameQuality::Ok
}

/// Letterbox the frame into the detector's square input and normalise the
/// way InsightFace does: `(pixel - 127.5) / 128`, the one grey channel
/// replicated into RGB. Returns the input and the scale applied, so detections
/// can be mapped back to frame coordinates.
///
/// The frame is scaled to fit and anchored at the top-left with zero padding
/// below or beside it, matching `insightface.model_zoo.SCRFD.detect`.
fn preprocess_for_detector(frame: &IrFrame) -> anyhow::Result<(tract_ndarray::Array3<f32>, f32)> {
    let img = crate::preprocess::frame_to_luma16(frame)?;
    let scale = DETECTOR_INPUT as f32 / frame.width.max(frame.height) as f32;
    let new_w = ((frame.width as f32 * scale).round() as u32).clamp(1, DETECTOR_INPUT as u32);
    let new_h = ((frame.height as f32 * scale).round() as u32).clamp(1, DETECTOR_INPUT as u32);

    // Triangle (bilinear) is plenty for a downscale feeding a detector and is
    // several times cheaper than Lanczos3.
    let resized = DynamicImage::ImageLuma16(img)
        .resize_exact(new_w, new_h, image::imageops::FilterType::Triangle)
        .to_luma8();

    // Zero padding is (0 - 127.5) / 128 after normalisation.
    let pad = (0.0 - 127.5) / 128.0;
    let mut array = tract_ndarray::Array3::<f32>::from_elem((3, DETECTOR_INPUT, DETECTOR_INPUT), pad);
    for y in 0..new_h as usize {
        for x in 0..new_w as usize {
            let v = (resized.get_pixel(x as u32, y as u32).0[0] as f32 - 127.5) / 128.0;
            array[[0, y, x]] = v;
            array[[1, y, x]] = v;
            array[[2, y, x]] = v;
        }
    }

    Ok((array, scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a frame from 8-bit values, widened the way `capture_frame` does.
    fn frame8(values: Vec<u8>, width: u32, height: u32) -> IrFrame {
        IrFrame {
            data: values.into_iter().map(|v| v as u16 * 257).collect(),
            width,
            height,
        }
    }

    #[test]
    fn empty_frame_is_rejected() {
        assert_eq!(assess_frame(&frame8(vec![], 0, 0)), FrameQuality::Empty);
    }

    #[test]
    fn covered_lens_is_too_flat() {
        let q = assess_frame(&frame8(vec![128; 1024], 32, 32));
        assert!(matches!(q, FrameQuality::TooFlat { .. }), "got {q:?}");
    }

    #[test]
    fn unlit_strobe_frame_is_too_dark() {
        let data: Vec<u8> = (0..1024).map(|i| (i % 17) as u8).collect();
        let q = assess_frame(&frame8(data, 32, 32));
        assert!(matches!(q, FrameQuality::TooDark { .. }), "got {q:?}");
    }

    #[test]
    fn illuminated_frame_is_accepted() {
        let data: Vec<u8> = (0..1024).map(|i| (i % 128) as u8).collect();
        assert_eq!(assess_frame(&frame8(data, 32, 32)), FrameQuality::Ok);
    }

    #[test]
    fn brightness_alone_does_not_pass() {
        let q = assess_frame(&frame8(vec![200; 1024], 32, 32));
        assert!(matches!(q, FrameQuality::TooFlat { .. }), "got {q:?}");
    }

    #[test]
    fn letterbox_scales_to_fit_and_pads_with_black() {
        // 64x40 frame of mid grey: scaled by 5 to 320x200, padded below.
        let f = frame8(vec![128; 64 * 40], 64, 40);
        let (arr, scale) = preprocess_for_detector(&f).unwrap();
        assert!((scale - 5.0).abs() < 1e-6);
        assert_eq!(arr.shape(), &[3, DETECTOR_INPUT, DETECTOR_INPUT]);
        let mid = (128.0 - 127.5) / 128.0;
        assert!((arr[[0, 100, 100]] - mid).abs() < 1e-5, "image area");
        let pad = (0.0 - 127.5) / 128.0;
        assert!((arr[[0, 250, 100]] - pad).abs() < 1e-5, "padding below the image");
        assert_eq!(arr[[0, 100, 100]], arr[[2, 100, 100]], "grey replicated across channels");
    }

    #[test]
    fn rejects_frame_shorter_than_its_geometry() {
        let f = IrFrame { data: vec![0; 10], width: 32, height: 32 };
        assert!(preprocess_for_detector(&f).is_err());
    }
}
