//! Unrotated PDF user space (points, bottom-left) ↔ cropped display pixels.
//! Quads retain PDF order: top-left, top-right, bottom-left, bottom-right (Z order).
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
    pub top: f32,
}
impl Rect {
    pub fn valid(self) -> bool {
        [self.left, self.bottom, self.right, self.top]
            .iter()
            .all(|v| v.is_finite())
            && self.right > self.left
            && self.top > self.bottom
    }
    pub fn width(self) -> f32 {
        self.right - self.left
    }
    pub fn height(self) -> f32 {
        self.top - self.bottom
    }
    pub fn quad(self) -> Quad {
        Quad([
            Point {
                x: self.left,
                y: self.top,
            },
            Point {
                x: self.right,
                y: self.top,
            },
            Point {
                x: self.left,
                y: self.bottom,
            },
            Point {
                x: self.right,
                y: self.bottom,
            },
        ])
    }
    pub fn contains(self, p: Point) -> bool {
        p.x >= self.left && p.x <= self.right && p.y >= self.bottom && p.y <= self.top
    }
    pub fn intersection(self, other: Self) -> Option<Self> {
        let r = Self {
            left: self.left.max(other.left),
            bottom: self.bottom.max(other.bottom),
            right: self.right.min(other.right),
            top: self.top.min(other.top),
        };
        r.valid().then_some(r)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Quad(pub [Point; 4]);
impl Quad {
    pub fn bounds(self) -> Rect {
        Rect {
            left: self.0.iter().map(|p| p.x).fold(f32::INFINITY, f32::min),
            right: self.0.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max),
            bottom: self.0.iter().map(|p| p.y).fold(f32::INFINITY, f32::min),
            top: self.0.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max),
        }
    }
    pub fn valid(self) -> bool {
        self.0.iter().all(|p| p.x.is_finite() && p.y.is_finite()) && self.bounds().valid()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rotation {
    R0,
    R90,
    R180,
    R270,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    TopLeft,
    BottomLeft,
}
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    crop: Rect,
    rotation: Rotation,
    zoom: f32,
    origin: Origin,
}
impl Viewport {
    /// Zoom is pixels per PDF point, including device scale. Rotation is clockwise.
    pub fn new(crop: Rect, rotation: Rotation, zoom: f32, origin: Origin) -> Option<Self> {
        (crop.valid()
            && zoom.is_finite()
            && zoom > 0.0
            && (crop.width() * zoom).is_finite()
            && (crop.height() * zoom).is_finite())
        .then_some(Self {
            crop,
            rotation,
            zoom,
            origin,
        })
    }
    pub fn size(self) -> (f32, f32) {
        let (w, h) = (
            self.crop.width() * self.zoom,
            self.crop.height() * self.zoom,
        );
        match self.rotation {
            Rotation::R0 | Rotation::R180 => (w, h),
            _ => (h, w),
        }
    }
    pub fn to_display(self, p: Point) -> Point {
        let (x, y) = (p.x - self.crop.left, p.y - self.crop.bottom);
        let (w, h) = (self.crop.width(), self.crop.height());
        let (x, y) = match self.rotation {
            Rotation::R0 => (x, h - y),
            Rotation::R90 => (y, x),
            Rotation::R180 => (w - x, y),
            Rotation::R270 => (h - y, w - x),
        };
        Point {
            x: x * self.zoom,
            y: match self.origin {
                Origin::TopLeft => y * self.zoom,
                Origin::BottomLeft => self.size().1 - y * self.zoom,
            },
        }
    }
    pub fn to_pdf(self, p: Point) -> Point {
        let x = p.x / self.zoom;
        let y = match self.origin {
            Origin::TopLeft => p.y,
            Origin::BottomLeft => self.size().1 - p.y,
        } / self.zoom;
        let (w, h) = (self.crop.width(), self.crop.height());
        let (x, y) = match self.rotation {
            Rotation::R0 => (x, h - y),
            Rotation::R90 => (y, x),
            Rotation::R180 => (w - x, y),
            Rotation::R270 => (w - y, h - x),
        };
        Point {
            x: x + self.crop.left,
            y: y + self.crop.bottom,
        }
    }
    pub fn quad_to_display(self, q: Quad) -> Quad {
        Quad(q.0.map(|p| self.to_display(p)))
    }
    /// Boundary points are inclusive; points outside the crop must not select another page.
    pub fn hit_test(self, p: Point) -> Option<Point> {
        if !p.x.is_finite() || !p.y.is_finite() {
            return None;
        }
        let (w, h) = self.size();
        (p.x >= 0.0 && p.y >= 0.0 && p.x <= w && p.y <= h).then(|| self.to_pdf(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nonzero_crop_all_rotations_zoom_and_origins() {
        let crop = Rect {
            left: 20.,
            bottom: 30.,
            right: 220.,
            top: 330.,
        };
        for rotation in [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270] {
            for zoom in [0.5, 1., 2.5] {
                for origin in [Origin::TopLeft, Origin::BottomLeft] {
                    let v = Viewport::new(crop, rotation, zoom, origin).unwrap();
                    for p in crop.quad().0.into_iter().chain([Point { x: 73., y: 125. }]) {
                        let d = v.to_display(p);
                        let r = v.to_pdf(d);
                        assert!((p.x - r.x).abs() < 0.0001 && (p.y - r.y).abs() < 0.0001);
                        assert!(v.hit_test(d).is_some());
                    }
                    assert!(v.hit_test(Point { x: -0.01, y: 0. }).is_none());
                    assert!(
                        v.hit_test(Point {
                            x: v.size().0 + 0.01,
                            y: 0.
                        })
                        .is_none()
                    );
                    assert!(v.hit_test(Point { x: f32::NAN, y: 0. }).is_none());
                }
            }
        }
        // Independent physical corner expectations, not just inverse-function checks.
        let p = Point { x: 20., y: 330. };
        for (r, expected) in [
            (Rotation::R0, (0., 0.)),
            (Rotation::R90, (600., 0.)),
            (Rotation::R180, (400., 600.)),
            (Rotation::R270, (0., 400.)),
        ] {
            let d = Viewport::new(crop, r, 2., Origin::TopLeft)
                .unwrap()
                .to_display(p);
            assert_eq!((d.x, d.y), expected);
        }
        assert!(Viewport::new(crop, Rotation::R0, 0., Origin::TopLeft).is_none());
    }
}
