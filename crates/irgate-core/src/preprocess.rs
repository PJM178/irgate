use crate::capture::IrFrame;
use image::{ImageBuffer, Luma};
use tract_onnx::prelude::tract_ndarray::Array3;

const ENCODER_SIZE: usize = 112;

/// Wrap a captured frame as a 16-bit greyscale image.
///
/// Shared by the detector and encoder paths so the geometry check lives in
/// one place: `ImageBuffer::from_fn` indexes `width * height` pixels, and a
/// frame shorter than that would otherwise be silently zero-padded into a
/// picture that is part real and part black.
pub fn frame_to_luma16(frame: &IrFrame) -> anyhow::Result<ImageBuffer<Luma<u16>, Vec<u16>>> {
    let width = frame.width;
    let height = frame.height;
    anyhow::ensure!(width > 0 && height > 0, "frame has zero extent");

    let expected = width as usize * height as usize;
    anyhow::ensure!(
        frame.data.len() >= expected,
        "frame holds {} samples, expected {} for {}x{}",
        frame.data.len(),
        expected,
        width,
        height
    );

    Ok(ImageBuffer::from_fn(width, height, |x, y| {
        Luma([frame.data[(y * width + x) as usize]])
    }))
}

/// ArcFace's canonical landmark positions in a 112x112 crop: left eye, right
/// eye, nose tip, left and right mouth corners. Every InsightFace recognition
/// model, `w600k_mbf` included, was trained on faces warped so that their
/// landmarks land here. Feeding it anything else — a loose crop, let alone a
/// whole frame — throws away most of what it learned.
pub const ARCFACE_LANDMARKS: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

/// A 2-D similarity transform `[a, -b, tx; b, a, ty]`: uniform scale and
/// rotation plus translation, no shear, no reflection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Similarity {
    pub a: f32,
    pub b: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Similarity {
    /// Least-squares fit mapping each `src[i]` onto `dst[i]`.
    ///
    /// This is the closed form of the four-parameter Procrustes problem,
    /// which is what `skimage.transform.SimilarityTransform` (Umeyama) gives
    /// for well-conditioned face landmarks. Returns `None` if the source
    /// points are degenerate (all coincident) or the fit is not finite.
    pub fn fit(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> Option<Self> {
        let n = src.len() as f32;
        let (mut mx, mut my, mut mu, mut mv) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for (s, d) in src.iter().zip(dst) {
            mx += s[0];
            my += s[1];
            mu += d[0];
            mv += d[1];
        }
        mx /= n;
        my /= n;
        mu /= n;
        mv /= n;

        let (mut sxx, mut sxu, mut sxv) = (0.0f32, 0.0f32, 0.0f32);
        for (s, d) in src.iter().zip(dst) {
            let (x, y) = (s[0] - mx, s[1] - my);
            let (u, v) = (d[0] - mu, d[1] - mv);
            sxx += x * x + y * y;
            sxu += x * u + y * v;
            sxv += x * v - y * u;
        }
        if !(sxx > 1e-6) {
            return None;
        }
        let a = sxu / sxx;
        let b = sxv / sxx;
        let tx = mu - (a * mx - b * my);
        let ty = mv - (b * mx + a * my);
        let t = Self { a, b, tx, ty };
        if [a, b, tx, ty].iter().all(|v| v.is_finite()) && t.scale() > 1e-6 {
            Some(t)
        } else {
            None
        }
    }

    pub fn scale(&self) -> f32 {
        (self.a * self.a + self.b * self.b).sqrt()
    }

    pub fn apply(&self, p: [f32; 2]) -> [f32; 2] {
        [
            self.a * p[0] - self.b * p[1] + self.tx,
            self.b * p[0] + self.a * p[1] + self.ty,
        ]
    }

    /// Map a destination point back to its source.
    pub fn invert(&self, q: [f32; 2]) -> [f32; 2] {
        let s2 = self.a * self.a + self.b * self.b;
        let (u, v) = (q[0] - self.tx, q[1] - self.ty);
        [
            (self.a * u + self.b * v) / s2,
            (-self.b * u + self.a * v) / s2,
        ]
    }
}

/// Bilinear sample of an 8-bit-widened u16 frame at a fractional position,
/// returned on the 0..=255 scale. Outside the frame reads as black, which is
/// what InsightFace's `cv2.warpAffine` default border gives.
fn sample_bilinear(frame: &IrFrame, x: f32, y: f32) -> f32 {
    let (w, h) = (frame.width as i64, frame.height as i64);
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let (x0, y0) = (x0 as i64, y0 as i64);
    let px = |xi: i64, yi: i64| -> f32 {
        if xi < 0 || yi < 0 || xi >= w || yi >= h {
            0.0
        } else {
            frame.data[(yi * w + xi) as usize] as f32 / 257.0
        }
    };
    let top = px(x0, y0) * (1.0 - fx) + px(x0 + 1, y0) * fx;
    let bottom = px(x0, y0 + 1) * (1.0 - fx) + px(x0 + 1, y0 + 1) * fx;
    top * (1.0 - fy) + bottom * fy
}

/// Warp the face described by `landmarks` into the encoder's `[3, 112, 112]`
/// input, aligned to [`ARCFACE_LANDMARKS`] and normalised to `[-1, 1]`
/// (`(pixel - 127.5) / 127.5`, as InsightFace does).
///
/// The arithmetic here defines what an enrolled embedding means; changing it
/// invalidates every template already on disk, which is why the template file
/// carries a format version.
pub fn align_face(frame: &IrFrame, landmarks: &[[f32; 2]; 5]) -> anyhow::Result<Array3<f32>> {
    anyhow::ensure!(frame.width > 0 && frame.height > 0, "frame has zero extent");
    anyhow::ensure!(
        frame.data.len() >= frame.width as usize * frame.height as usize,
        "frame holds {} samples, expected {} for {}x{}",
        frame.data.len(),
        frame.width as usize * frame.height as usize,
        frame.width,
        frame.height
    );
    let xform = Similarity::fit(landmarks, &ARCFACE_LANDMARKS)
        .ok_or_else(|| anyhow::anyhow!("degenerate face landmarks {landmarks:?}"))?;

    let mut array = Array3::<f32>::zeros((3, ENCODER_SIZE, ENCODER_SIZE));
    for v in 0..ENCODER_SIZE {
        for u in 0..ENCODER_SIZE {
            let [x, y] = xform.invert([u as f32, v as f32]);
            let p = sample_bilinear(frame, x, y);
            let n = (p - 127.5) / 127.5;
            array[[0, v, u]] = n;
            array[[1, v, u]] = n;
            array[[2, v, u]] = n;
        }
    }
    Ok(array)
}


/// Default CLAHE parameters. `CLIP_LIMIT` follows OpenCV's convention: the
/// per-bin ceiling is `clip * tile_pixels / 256`, with the clipped mass
/// redistributed. Howdy uses 2.0; 3.0 measured better on this sensor's frames
/// without visibly amplifying noise.
pub const CLAHE_CLIP_LIMIT: f32 = 3.0;
pub const CLAHE_TILES: u32 = 8;

/// Contrast-limited adaptive histogram equalisation.
///
/// Replaces the global equalisation this used to do. Global equalisation maps
/// one CDF over the whole frame, so a dark IR frame — where nearly all samples
/// sit in a narrow band — gets that band stretched across the full range,
/// turning sensor noise into hard posterised contours. Measured on this
/// camera, the face detector scored ~0.11 on globally-equalised frames (below
/// its 0.5 threshold, indistinguishable from an empty room) and 0.60-0.99 on
/// the same frames under CLAHE.
///
/// CLAHE instead equalises per tile with a ceiling on how much any one
/// intensity may be amplified, then bilinearly interpolates between
/// neighbouring tiles' mappings so no tile seams appear.
///
/// Samples are u16 carrying 8-bit data widened by 257 (see `capture_frame`),
/// so 256 bins are exact here.
pub fn clahe_equalize(frame: &mut IrFrame, clip_limit: f32, tiles: u32) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    if frame.data.is_empty() || w == 0 || h == 0 || frame.data.len() < w * h {
        return;
    }
    let tiles = tiles.max(1) as usize;
    let tile_w = w.div_ceil(tiles);
    let tile_h = h.div_ceil(tiles);

    // One 256-entry lookup table per tile.
    let mut luts = vec![[0u8; 256]; tiles * tiles];
    for ty in 0..tiles {
        for tx in 0..tiles {
            let x0 = tx * tile_w;
            let y0 = ty * tile_h;
            let x1 = (x0 + tile_w).min(w);
            let y1 = (y0 + tile_h).min(h);
            if x0 >= x1 || y0 >= y1 {
                continue;
            }

            let mut hist = [0u32; 256];
            for y in y0..y1 {
                for x in x0..x1 {
                    hist[(frame.data[y * w + x] >> 8) as usize] += 1;
                }
            }

            // Clip, then hand the excess back evenly. This is what bounds the
            // contrast gain and stops noise being amplified without limit.
            let count = ((x1 - x0) * (y1 - y0)) as f32;
            let limit = ((clip_limit * count) / 256.0).max(1.0) as u32;
            let mut excess: u32 = 0;
            for b in hist.iter_mut() {
                if *b > limit {
                    excess += *b - limit;
                    *b = limit;
                }
            }
            let share = excess / 256;
            let mut remainder = excess % 256;
            for b in hist.iter_mut() {
                *b += share;
                if remainder > 0 {
                    *b += 1;
                    remainder -= 1;
                }
            }

            let lut = &mut luts[ty * tiles + tx];
            let total = count.max(1.0);
            let mut cumulative = 0u32;
            for (i, &b) in hist.iter().enumerate() {
                cumulative += b;
                lut[i] = ((cumulative as f32 / total) * 255.0).clamp(0.0, 255.0) as u8;
            }
        }
    }

    // Bilinear blend between the four nearest tile centres.
    for y in 0..h {
        let gy = ((y as f32 - tile_h as f32 * 0.5) / tile_h as f32).max(0.0);
        let ty0 = (gy as usize).min(tiles - 1);
        let ty1 = (ty0 + 1).min(tiles - 1);
        let fy = gy - ty0 as f32;

        for x in 0..w {
            let gx = ((x as f32 - tile_w as f32 * 0.5) / tile_w as f32).max(0.0);
            let tx0 = (gx as usize).min(tiles - 1);
            let tx1 = (tx0 + 1).min(tiles - 1);
            let fx = gx - tx0 as f32;

            let v = (frame.data[y * w + x] >> 8) as usize;
            let tl = luts[ty0 * tiles + tx0][v] as f32;
            let tr = luts[ty0 * tiles + tx1][v] as f32;
            let bl = luts[ty1 * tiles + tx0][v] as f32;
            let br = luts[ty1 * tiles + tx1][v] as f32;

            let top = tl + (tr - tl) * fx;
            let bottom = bl + (br - bl) * fx;
            let out = (top + (bottom - top) * fy).clamp(0.0, 255.0) as u16;
            frame.data[y * w + x] = out * 257;
        }
    }
}

/// Equalise a frame for detection and encoding, using the project defaults.
pub fn histogram_equalize(frame: &mut IrFrame) {
    clahe_equalize(frame, CLAHE_CLIP_LIMIT, CLAHE_TILES);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(data: Vec<u16>, width: u32, height: u32) -> IrFrame {
        IrFrame { data, width, height }
    }

    #[test]
    fn rejects_frame_shorter_than_its_geometry() {
        // Previously padded with zeros, producing a half-black image that the
        // encoder would happily turn into an embedding.
        let f = frame(vec![0; 10], 32, 32);
        assert!(frame_to_luma16(&f).is_err());
        assert!(align_face(&f, &ARCFACE_LANDMARKS).is_err());
    }

    #[test]
    fn rejects_zero_extent_frame() {
        assert!(frame_to_luma16(&frame(vec![], 0, 0)).is_err());
    }

    #[test]
    fn accepts_exactly_sized_frame() {
        let f = frame(vec![1234; 32 * 32], 32, 32);
        let img = frame_to_luma16(&f).unwrap();
        assert_eq!(img.dimensions(), (32, 32));
    }

    #[test]
    fn accepts_frame_with_trailing_padding() {
        // Some drivers report bytesused beyond the visible image.
        let f = frame(vec![7; 32 * 32 + 64], 32, 32);
        assert!(frame_to_luma16(&f).is_ok());
    }

    #[test]
    fn similarity_fit_recovers_a_known_transform() {
        // Rotate 30°, scale 2, translate (10, -5); the fit must recover it.
        let (c, s) = (30f32.to_radians().cos() * 2.0, 30f32.to_radians().sin() * 2.0);
        let truth = Similarity { a: c, b: s, tx: 10.0, ty: -5.0 };
        let src = ARCFACE_LANDMARKS;
        let dst: [[f32; 2]; 5] = std::array::from_fn(|i| truth.apply(src[i]));
        let fit = Similarity::fit(&src, &dst).unwrap();
        for (k, (x, y)) in [(fit.a, truth.a), (fit.b, truth.b), (fit.tx, truth.tx), (fit.ty, truth.ty)].into_iter().enumerate() {
            assert!((x - y).abs() < 1e-3, "param {k}: {x} vs {y}");
        }
        for p in src {
            let q = fit.invert(fit.apply(p));
            assert!((q[0] - p[0]).abs() < 1e-3 && (q[1] - p[1]).abs() < 1e-3);
        }
    }

    #[test]
    fn similarity_fit_rejects_coincident_points() {
        let src = [[5.0, 5.0]; 5];
        assert!(Similarity::fit(&src, &ARCFACE_LANDMARKS).is_none());
    }

    #[test]
    fn align_face_puts_landmarks_where_arcface_expects() {
        // A frame with a bright dot at each landmark's source position; after
        // alignment the dots must sit at the canonical positions.
        let (w, h) = (640u32, 400u32);
        let mut f = frame(vec![0; (w * h) as usize], w, h);
        // Place the face at twice the canonical size, offset into the frame.
        let src: [[f32; 2]; 5] = std::array::from_fn(|i| {
            [ARCFACE_LANDMARKS[i][0] * 2.0 + 200.0, ARCFACE_LANDMARKS[i][1] * 2.0 + 100.0]
        });
        for p in src {
            for dy in -1..=1i32 {
                for dx in -1..=1i32 {
                    let x = (p[0].round() as i32 + dx) as usize;
                    let y = (p[1].round() as i32 + dy) as usize;
                    f.data[y * w as usize + x] = 255 * 257;
                }
            }
        }
        let arr = align_face(&f, &src).unwrap();
        assert_eq!(arr.shape(), &[3, ENCODER_SIZE, ENCODER_SIZE]);
        for p in ARCFACE_LANDMARKS {
            let (u, v) = (p[0].round() as usize, p[1].round() as usize);
            assert!(arr[[0, v, u]] > 0.5, "landmark at ({u},{v}) not bright: {}", arr[[0, v, u]]);
        }
        // Far corner stays black.
        assert!(arr[[0, 0, 0]] < -0.9);
        assert!(arr.iter().all(|v| v.is_finite() && (-1.0..=1.0).contains(v)));
    }

    #[test]
    fn clahe_expands_a_low_contrast_frame() {
        // A dark, narrow-range frame — what an unlit-ish IR capture looks like.
        let data: Vec<u16> = (0..64 * 64).map(|i| ((i % 20) as u16 + 20) * 257).collect();
        let mut f = frame(data, 64, 64);
        let before = spread(&f);
        histogram_equalize(&mut f);
        assert!(spread(&f) > before, "CLAHE should widen the tonal range");
        assert!(f.data.iter().all(|&v| v % 257 == 0), "output stays 8-bit widened");
    }

    #[test]
    fn clahe_leaves_a_flat_frame_flat() {
        // Uniform input has no contrast to recover; it must not explode into
        // noise, which is precisely what the clip limit is for.
        let mut f = frame(vec![128 * 257; 64 * 64], 64, 64);
        histogram_equalize(&mut f);
        let first = f.data[0];
        assert!(f.data.iter().all(|&v| v == first));
    }

    #[test]
    fn clahe_handles_degenerate_frames() {
        let mut empty = frame(vec![], 0, 0);
        histogram_equalize(&mut empty);
        assert!(empty.data.is_empty());

        // Short buffer must be left alone rather than indexed out of bounds.
        let mut short = frame(vec![100; 10], 32, 32);
        histogram_equalize(&mut short);
        assert_eq!(short.data.len(), 10);
    }

    fn spread(f: &IrFrame) -> u16 {
        let max = f.data.iter().copied().max().unwrap_or(0);
        let min = f.data.iter().copied().min().unwrap_or(0);
        max - min
    }
}
