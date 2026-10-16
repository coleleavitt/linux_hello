//! Windows Hello anti-spoofing preprocessing.
//!
//! Re-implementation of `FaceRecognitionEngineAdapter.dll` input preparation:
//! - `align_crop`: 5-point similarity warp to 281×281, then resample to 128×128
//! - `thumbnail`: whole frame stretched to 128×128

use image::GrayImage;

/// Template landmarks in the 281×281 aligned face space.
/// Source: `unk_1800D7F68` in `FaceRecognitionEngineAdapter.dll`.
const TEMPLATE_281: [[f32; 2]; 5] = [
    [97.82990264892578, 127.7791976928711],   // left eye (image left)
    [181.05320739746094, 126.33809661865234], // right eye
    [139.57400512695312, 179.00059509277344], // nose tip
    [102.69640350341797, 216.70140075683594], // left mouth corner
    [179.30679321289062, 215.37330627441406], // right mouth corner
];

const ALIGN_SIZE: usize = 281;
const OUTPUT_SIZE: usize = 128;

/// Compute the 2×3 similarity transform from source landmarks to the template.
///
/// Least-squares fit: u = a·x + b·y + tx, v = −b·x + a·y + ty (src → dst).
/// Returns `[[a, b, tx], [-b, a, ty]]`.
fn fit_similarity(src: &[[f32; 2]; 5]) -> [[f32; 3]; 2] {
    let dst = &TEMPLATE_281;
    let n = 5.0f32;

    let (mut mx, mut my, mut mu, mut mv) = (0.0f32, 0.0, 0.0, 0.0);
    for i in 0..5 {
        mx += src[i][0];
        my += src[i][1];
        mu += dst[i][0];
        mv += dst[i][1];
    }
    mx /= n;
    my /= n;
    mu /= n;
    mv /= n;

    let mut sxx = 0.0f32;
    let mut sp = 0.0f32;
    let mut sm = 0.0f32;
    for i in 0..5 {
        let (x, y) = (src[i][0], src[i][1]);
        let (u, v) = (dst[i][0], dst[i][1]);
        sxx += x * x + y * y;
        sp += u * x + v * y;
        sm += u * y - v * x;
    }
    sxx /= n;
    sp /= n;
    sm /= n;

    let var = sxx - mx * mx - my * my;
    if var.abs() < 1e-6 {
        // Degenerate landmarks: return identity-ish transform
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    }

    let a = (sp - mx * mu - my * mv) / var;
    let b = (sm - my * mu + mx * mv) / var;
    let tx = (-my * sm - mx * sp + sxx * mu) / var;
    let ty = (-my * sp + mx * sm + sxx * mv) / var;

    [[a, b, tx], [-b, a, ty]]
}

/// Invert a 2×3 affine matrix (extended to 3×3 with [0, 0, 1]).
fn invert_affine(m: &[[f32; 3]; 2]) -> [[f32; 3]; 2] {
    let a = m[0][0] as f64;
    let b = m[0][1] as f64;
    let tx = m[0][2] as f64;
    let c = m[1][0] as f64;
    let d = m[1][1] as f64;
    let ty = m[1][2] as f64;

    let det = a * d - b * c;
    if det.abs() < 1e-12 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    }

    let inv_det = 1.0 / det;
    let ia = (d * inv_det) as f32;
    let ib = (-b * inv_det) as f32;
    let ic = (-c * inv_det) as f32;
    let id = (a * inv_det) as f32;
    let itx = ((b * ty - d * tx) * inv_det) as f32;
    let ity = ((c * tx - a * ty) * inv_det) as f32;

    [[ia, ib, itx], [ic, id, ity]]
}

/// Bilinear sample with Windows Hello rules: outside [0, w-1]×[0, h-1] → 0, truncated to u8.
fn bilinear_sample(img: &GrayImage, sx: f32, sy: f32) -> u8 {
    let (w, h) = (img.width() as f32, img.height() as f32);

    if sx < 0.0 || sx > w - 1.0 || sy < 0.0 || sy > h - 1.0 {
        return 0;
    }

    let x0 = sx.floor() as u32;
    let y0 = sy.floor() as u32;
    let x1 = (x0 + 1).min(img.width() - 1);
    let y1 = (y0 + 1).min(img.height() - 1);

    let fx = sx - x0 as f32;
    let fy = sy - y0 as f32;
    let gx = 1.0 - fx;
    let gy = 1.0 - fy;

    let p00 = img.get_pixel(x0, y0).0[0] as f32;
    let p01 = img.get_pixel(x1, y0).0[0] as f32;
    let p10 = img.get_pixel(x0, y1).0[0] as f32;
    let p11 = img.get_pixel(x1, y1).0[0] as f32;

    let val = p00 * gy * gx + p01 * gy * fx + p10 * fy * gx + p11 * fy * fx;
    val.trunc() as u8
}

/// Warp the image to 281×281 using inverse mapping: out(x, y) = img(inv(M) @ (x, y, 1)).
fn warp_281(img: &GrayImage, m: &[[f32; 3]; 2]) -> GrayImage {
    let inv = invert_affine(m);
    let mut out = GrayImage::new(ALIGN_SIZE as u32, ALIGN_SIZE as u32);

    for y in 0..ALIGN_SIZE {
        for x in 0..ALIGN_SIZE {
            let xf = x as f32;
            let yf = y as f32;
            let sx = xf * inv[0][0] + yf * inv[0][1] + inv[0][2];
            let sy = xf * inv[1][0] + yf * inv[1][1] + inv[1][2];
            out.put_pixel(x as u32, y as u32, image::Luma([bilinear_sample(img, sx, sy)]));
        }
    }

    out
}

/// Resample the central 230×230 of the 281×281 image to 128×128.
/// Source formula: src = (i - out/2) * 230/(out-1) + 140.5.
fn resize_281_to_128(img281: &GrayImage) -> GrayImage {
    let mut out = GrayImage::new(OUTPUT_SIZE as u32, OUTPUT_SIZE as u32);
    let half = (OUTPUT_SIZE / 2) as f32;
    let scale = 230.0 / (OUTPUT_SIZE as f32 - 1.0);

    for y in 0..OUTPUT_SIZE {
        for x in 0..OUTPUT_SIZE {
            let sx = (x as f32 - half) * scale + 140.5;
            let sy = (y as f32 - half) * scale + 140.5;
            out.put_pixel(x as u32, y as u32, image::Luma([bilinear_sample(img281, sx, sy)]));
        }
    }

    out
}

/// Align and crop a face using 5-point landmarks.
///
/// The full pipeline from `sub_180015F68`:
/// 1. Fit a similarity transform from landmarks to the 281×281 template
/// 2. Warp to 281×281 using inverse mapping
/// 3. Resample the central 230×230 to 128×128
pub fn align_crop(img: &GrayImage, landmarks: &[[f32; 2]; 5]) -> GrayImage {
    let m = fit_similarity(landmarks);
    let warped = warp_281(img, &m);
    resize_281_to_128(&warped)
}

/// Thumbnail: stretch the whole frame to 128×128 (no aspect ratio preservation).
///
/// Source: `FaceProcessorCore ResizeImage`, formula: src = (i - 63.5) * (W/128) + (W-1)/2.
pub fn thumbnail(frame: &GrayImage) -> GrayImage {
    let (w, h) = (frame.width() as f32, frame.height() as f32);
    let mut out = GrayImage::new(OUTPUT_SIZE as u32, OUTPUT_SIZE as u32);

    for y in 0..OUTPUT_SIZE {
        for x in 0..OUTPUT_SIZE {
            let sx = (x as f32 - 63.5) * (w * 0.0078125) + (w - 1.0) * 0.5;
            let sy = (y as f32 - 63.5) * (h * 0.0078125) + (h - 1.0) * 0.5;
            out.put_pixel(x as u32, y as u32, image::Luma([bilinear_sample(frame, sx, sy)]));
        }
    }

    out
}

/// Compute the difference image: lit - min(lit, ambient).
///
/// Source: `FaceProcessorCore SubtractAmbientFrameFromIlluminatedFrame<uchar>`.
pub fn difference_image(lit: &GrayImage, ambient: &GrayImage) -> GrayImage {
    assert_eq!(lit.dimensions(), ambient.dimensions());
    let (w, h) = lit.dimensions();
    let mut out = GrayImage::new(w, h);

    for y in 0..h {
        for x in 0..w {
            let l = lit.get_pixel(x, y).0[0] as i16;
            let a = ambient.get_pixel(x, y).0[0] as i16;
            let diff = (l - l.min(a)).max(0) as u8;
            out.put_pixel(x, y, image::Luma([diff]));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fit_similarity_identity_ish() {
        // When landmarks match template, transform should be near identity
        let src: [[f32; 2]; 5] = TEMPLATE_281;
        let m = fit_similarity(&src);
        // a should be ~1, b should be ~0
        assert!((m[0][0] - 1.0).abs() < 0.01);
        assert!(m[0][1].abs() < 0.01);
    }

    #[test]
    fn test_thumbnail_dimensions() {
        let frame = GrayImage::new(640, 480);
        let thumb = thumbnail(&frame);
        assert_eq!(thumb.dimensions(), (128, 128));
    }

    #[test]
    fn test_difference_image() {
        let mut lit = GrayImage::new(4, 4);
        let mut ambient = GrayImage::new(4, 4);
        lit.put_pixel(0, 0, image::Luma([100]));
        ambient.put_pixel(0, 0, image::Luma([30]));
        let diff = difference_image(&lit, &ambient);
        assert_eq!(diff.get_pixel(0, 0).0[0], 70);
    }
}
