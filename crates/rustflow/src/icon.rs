//! Tray icon drawn in code: two overlapping screens tinted by status.

use crate::status::Light;

fn color(light: Light) -> [u8; 3] {
    match light {
        Light::Connected => [52, 199, 89],
        Light::Waiting => [255, 159, 10],
        Light::Error => [255, 69, 58],
        Light::Paused => [142, 142, 147],
    }
}

/// RGBA pixels, `size` × `size`.
pub fn rgba(size: u32, light: Light) -> Vec<u8> {
    let s = size as f32;
    let [r, g, b] = color(light);
    let mut px = vec![0u8; (size * size * 4) as usize];

    // Back screen (outline) top-left, front screen (filled) bottom-right.
    let back = Rect { x0: 0.08, y0: 0.10, x1: 0.68, y1: 0.62 }.scale(s);
    let front = Rect { x0: 0.32, y0: 0.38, x1: 0.92, y1: 0.90 }.scale(s);
    let stroke = (s * 0.09).max(1.5);
    let radius = s * 0.12;

    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let i = ((y * size + x) * 4) as usize;
            let front_cov = front.coverage(fx, fy, radius);
            let back_cov = back.coverage(fx, fy, radius) * (1.0 - back.inset(stroke).coverage(fx, fy, radius));
            // The front screen hides the back screen where they overlap.
            let back_cov = back_cov * (1.0 - front.grow(stroke * 0.8).coverage(fx, fy, radius));
            let a = (front_cov + back_cov).min(1.0);
            if a > 0.0 {
                px[i..i + 4].copy_from_slice(&[r, g, b, (a * 255.0) as u8]);
            }
        }
    }
    px
}

#[derive(Clone, Copy)]
struct Rect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Rect {
    fn scale(self, s: f32) -> Self {
        Self { x0: self.x0 * s, y0: self.y0 * s, x1: self.x1 * s, y1: self.y1 * s }
    }

    fn inset(self, d: f32) -> Self {
        Self { x0: self.x0 + d, y0: self.y0 + d, x1: self.x1 - d, y1: self.y1 - d }
    }

    fn grow(self, d: f32) -> Self {
        self.inset(-d)
    }

    /// Anti-aliased coverage of a rounded rectangle at a pixel centre.
    fn coverage(self, x: f32, y: f32, r: f32) -> f32 {
        let r = r.min((self.x1 - self.x0) / 2.0).min((self.y1 - self.y0) / 2.0).max(0.0);
        let cx = x.clamp(self.x0 + r, self.x1 - r);
        let cy = y.clamp(self.y0 + r, self.y1 - r);
        let dist = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r;
        (0.5 - dist).clamp(0.0, 1.0)
    }
}

/// ARGB32 big-endian, as the StatusNotifierItem spec wants.
#[cfg(target_os = "linux")]
pub fn argb(size: u32, light: Light) -> Vec<u8> {
    rgba(size, light)
        .chunks_exact(4)
        .flat_map(|p| [p[3], p[0], p[1], p[2]])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_something() {
        let px = rgba(32, Light::Connected);
        assert_eq!(px.len(), 32 * 32 * 4);
        assert!(px.chunks_exact(4).any(|p| p[3] == 255));
        assert!(px.chunks_exact(4).any(|p| p[3] == 0));
    }
}
