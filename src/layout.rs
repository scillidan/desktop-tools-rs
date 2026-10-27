//! Pure snapping geometry. Ported from RectangleWin `snap.go`
//! (fraction-of-workarea rect math; no Win32 types in here).

use anyhow::{Context, Result};
use std::fmt;
use std::str::FromStr;

/// Inclusive-exclusive rect, matching Win32 RECT semantics (right/bottom).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl Rect {
    pub fn w(&self) -> i32 {
        self.r - self.l
    }
    pub fn h(&self) -> i32 {
        self.b - self.t
    }

    pub fn has_area(&self) -> bool {
        self.w() > 0 && self.h() > 0
    }
}

/// A Sizer preset: exact pixels, or a fraction applied to both work-area
/// dimensions (width and height).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SizeSpec {
    Px { w: i32, h: i32 },
    Fraction { num: u32, den: u32 },
}

impl SizeSpec {
    pub fn resolve(self, work: Rect) -> (i32, i32) {
        match self {
            SizeSpec::Px { w, h } => (w, h),
            SizeSpec::Fraction { num, den } => {
                let w = (work.w() as i64 * num as i64 / den as i64) as i32;
                let h = (work.h() as i64 * num as i64 / den as i64) as i32;
                (w.max(1), h.max(1))
            }
        }
    }
}

impl FromStr for SizeSpec {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        if let Some((w, h)) = s.split_once(['x', 'X']) {
            let w: i32 = w
                .trim()
                .parse()
                .with_context(|| format!("invalid width in `{s}`"))?;
            let h: i32 = h
                .trim()
                .parse()
                .with_context(|| format!("invalid height in `{s}`"))?;
            anyhow::ensure!((1..=20_000).contains(&w), "width must be 1..=20000");
            anyhow::ensure!((1..=20_000).contains(&h), "height must be 1..=20000");
            return Ok(SizeSpec::Px { w, h });
        }
        let (num, den) = s
            .split_once('/')
            .with_context(|| format!("preset `{s}` must be WxH (1600x1200) or N/D (1/2)"))?;
        let num: u32 = num
            .trim()
            .parse()
            .with_context(|| format!("invalid numerator in `{s}`"))?;
        let den: u32 = den
            .trim()
            .parse()
            .with_context(|| format!("invalid denominator in `{s}`"))?;
        anyhow::ensure!(
            num > 0 && num <= den && den <= 1000,
            "fraction must be 1 <= N <= D <= 1000"
        );
        Ok(SizeSpec::Fraction { num, den })
    }
}

impl fmt::Display for SizeSpec {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            SizeSpec::Px { w, h } => write!(f, "{w}x{h}"),
            SizeSpec::Fraction { num, den } => write!(f, "{num}/{den}"),
        }
    }
}

/// Intersection; empty (zero-area) results are fine, callers check `has_area`.
/// (Only the X11 backend uses this today — panels-in-work-area math.)
#[cfg_attr(windows, allow(dead_code))]
pub fn intersect(a: Rect, b: Rect) -> Rect {
    Rect {
        l: a.l.max(b.l),
        t: a.t.max(b.t),
        r: a.r.min(b.r),
        b: a.b.min(b.b),
    }
}

pub fn to_left(d: Rect, mul: i32, div: i32) -> Rect {
    let w = (d.w() * mul + div - 1) / div; // ceil, matches RectangleWin math.Ceil
    Rect {
        l: d.l,
        t: d.t,
        r: d.l + w,
        b: d.b,
    }
}

pub fn to_right(d: Rect, mul: i32, div: i32) -> Rect {
    let w = (d.w() * mul + div - 1) / div;
    Rect {
        l: d.r - w,
        t: d.t,
        r: d.r,
        b: d.b,
    }
}

pub fn to_top(d: Rect, mul: i32, div: i32) -> Rect {
    let h = (d.h() * mul + div - 1) / div;
    Rect {
        l: d.l,
        t: d.t,
        r: d.r,
        b: d.t + h,
    }
}

pub fn to_bottom(d: Rect, mul: i32, div: i32) -> Rect {
    let h = (d.h() * mul + div - 1) / div;
    Rect {
        l: d.l,
        t: d.b - h,
        r: d.r,
        b: d.b,
    }
}

/// RectangleWin `merge`: horizontal edges from `x`, vertical edges from `y`.
pub fn merge(x: Rect, y: Rect) -> Rect {
    Rect {
        l: x.l,
        t: y.t,
        r: x.r,
        b: y.b,
    }
}

/// Shrink `d` by `n` pixels on every side (the `gap` setting). Clamped so the
/// result keeps a positive area even for a huge gap.
pub fn inset(d: Rect, n: i32) -> Rect {
    let n = n.max(0).min(((d.w().min(d.h()) - 1) / 2).max(0));
    Rect {
        l: d.l + n,
        t: d.t + n,
        r: d.r - n,
        b: d.b - n,
    }
}

/// Center `cur` inside `disp`, keeping its size.
pub fn center(disp: Rect, cur: Rect) -> Rect {
    let w = (disp.w() - cur.w()) / 2;
    let h = (disp.h() - cur.h()) / 2;
    Rect {
        l: disp.l + w,
        r: disp.l + w + cur.w(),
        t: disp.t + h,
        b: disp.t + h + cur.h(),
    }
}

/// Grow or shrink `cur` by the `from` -> `to` work-area ratio, uniformly in
/// both axes so the window keeps its aspect ratio; clamped to `to`.
pub fn scale_to(cur: Rect, from: Rect, to: Rect) -> Rect {
    let s = (to.w() as f64 / from.w().max(1) as f64).min(to.h() as f64 / from.h().max(1) as f64);
    let w = ((cur.w() as f64 * s).round() as i32).clamp(1, to.w().max(1));
    let h = ((cur.h() as f64 * s).round() as i32).clamp(1, to.h().max(1));
    Rect {
        l: cur.l,
        t: cur.t,
        r: cur.l + w,
        b: cur.t + h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: Rect = Rect {
        l: 100,
        t: 50,
        r: 1100,
        b: 750,
    }; // 1000x700

    #[test]
    fn fractions() {
        assert_eq!(
            to_left(D, 1, 2),
            Rect {
                l: 100,
                t: 50,
                r: 600,
                b: 750
            }
        );
        assert_eq!(
            to_right(D, 2, 3),
            Rect {
                l: 433,
                t: 50,
                r: 1100,
                b: 750
            }
        );
        assert_eq!(
            to_top(D, 1, 3),
            Rect {
                l: 100,
                t: 50,
                r: 1100,
                b: 284
            }
        );
        assert_eq!(
            to_bottom(D, 1, 2),
            Rect {
                l: 100,
                t: 400,
                r: 1100,
                b: 750
            }
        );
    }

    #[test]
    fn merge_takes_horizontal_from_first() {
        let x = to_left(D, 1, 2);
        let y = to_top(D, 1, 2);
        assert_eq!(
            merge(x, y),
            Rect {
                l: 100,
                t: 50,
                r: 600,
                b: 400
            }
        );
    }

    #[test]
    fn inset_shrinks_all_sides() {
        assert_eq!(
            inset(D, 20),
            Rect {
                l: 120,
                t: 70,
                r: 1080,
                b: 730
            }
        );
        assert_eq!(inset(D, 0), D);
        assert_eq!(inset(D, -5), D);
        let huge = inset(D, 10_000);
        assert!(huge.has_area());
        assert_eq!(huge.l - D.l, D.r - huge.r);
        assert_eq!(huge.t - D.t, D.b - huge.b);
    }

    #[test]
    fn center_keeps_size() {
        let cur = Rect {
            l: 0,
            t: 0,
            r: 200,
            b: 100,
        };
        let c = center(D, cur);
        assert_eq!(c.w(), 200);
        assert_eq!(c.h(), 100);
        assert_eq!(c.l + c.r, D.l + D.r); // centered on odd slack too
        assert_eq!(c.t + c.b, D.t + D.b);
    }

    #[test]
    fn scale_to_keeps_aspect() {
        // 1000x700 -> 2000x1050: the height limits the scale to 1.5x.
        let to = Rect {
            l: 1000,
            t: 0,
            r: 3000,
            b: 1050,
        };
        let s = scale_to(D, D, to);
        assert_eq!((s.w(), s.h()), (1500, 1050));
    }

    #[test]
    fn size_spec_parses_and_resolves() {
        let s: SizeSpec = "1280x720".parse().unwrap();
        assert_eq!(s.resolve(D), (1280, 720));
        assert_eq!(s.to_string(), "1280x720");
        let s: SizeSpec = "1280X720".parse().unwrap();
        assert_eq!(s.resolve(D), (1280, 720));

        let s: SizeSpec = "1/2".parse().unwrap();
        assert_eq!(s.resolve(D), (500, 350)); // both dimensions
        assert_eq!(s.to_string(), "1/2");
        let s: SizeSpec = "3/4".parse().unwrap();
        assert_eq!(s.resolve(D), (750, 525));

        assert!("50x".parse::<SizeSpec>().is_err());
        assert!("0x100".parse::<SizeSpec>().is_err());
        assert!("0/2".parse::<SizeSpec>().is_err());
        assert!("3/2".parse::<SizeSpec>().is_err());
        assert!("hugex100".parse::<SizeSpec>().is_err());
        assert!("abc".parse::<SizeSpec>().is_err());
    }

    #[test]
    fn intersect_clips() {
        let a = Rect {
            l: 0,
            t: 0,
            r: 10,
            b: 10,
        };
        let b = Rect {
            l: 5,
            t: -5,
            r: 15,
            b: 5,
        };
        assert_eq!(
            intersect(a, b),
            Rect {
                l: 5,
                t: 0,
                r: 10,
                b: 5
            }
        );
        assert!(!intersect(
            a,
            Rect {
                l: 20,
                t: 0,
                r: 30,
                b: 10
            }
        )
        .has_area());
    }
}
