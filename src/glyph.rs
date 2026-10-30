//! Threat silhouettes (same shapes and colours as the macOS app), nose pointing up in a -1..1 box.

use tiny_skia::{Color, Path, PathBuilder};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Uav,
    Recon,
    Fpv,
    Missile,
    Ballistic,
    Mig31k,
    Kab,
    Unknown,
}

impl Kind {
    pub const ALL: [Kind; 8] =
        [Kind::Uav, Kind::Recon, Kind::Fpv, Kind::Missile, Kind::Ballistic, Kind::Mig31k, Kind::Kab, Kind::Unknown];

    pub fn from(raw: &str) -> Kind {
        match raw {
            "uav" => Kind::Uav,
            "recon" => Kind::Recon,
            "fpv" => Kind::Fpv,
            "missile" => Kind::Missile,
            "ballistic" => Kind::Ballistic,
            "mig31k" => Kind::Mig31k,
            "kab" => Kind::Kab,
            _ => Kind::Unknown,
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Kind::Uav => "Шахед",
            Kind::Recon => "Розвідник",
            Kind::Fpv => "FPV",
            Kind::Missile => "Ракета",
            Kind::Ballistic => "Балістика",
            Kind::Mig31k => "Кинджал",
            Kind::Kab => "КАБ",
            Kind::Unknown => "Інше",
        }
    }

    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            Kind::Uav => (255, 59, 48),
            Kind::Recon => (255, 158, 10),
            Kind::Fpv => (255, 214, 10),
            Kind::Missile => (191, 90, 242),
            Kind::Ballistic => (255, 46, 158),
            Kind::Mig31k => (90, 200, 250),
            Kind::Kab => (52, 214, 74),
            Kind::Unknown => (158, 158, 158),
        }
    }

    pub fn color(self) -> Color {
        let (r, g, b) = self.rgb();
        Color::from_rgba8(r, g, b, 255)
    }

    /// Fast, high-danger types reported from far out (WATCH_KM).
    pub fn is_missile(self) -> bool {
        matches!(self, Kind::Missile | Kind::Ballistic | Kind::Mig31k)
    }

    /// KABs fall and FPVs hover: no course worth showing.
    pub fn shows_heading(self) -> bool {
        !matches!(self, Kind::Kab | Kind::Fpv | Kind::Unknown)
    }

    pub fn path(self) -> Path {
        let mut pb = PathBuilder::new();
        let mut poly = |pts: &[(f32, f32)]| {
            let mut all: Vec<(f32, f32)> = pts.to_vec();
            all.extend(pts.iter().rev().map(|&(x, y)| (-x, y)));
            pb.move_to(all[0].0, all[0].1);
            for &(x, y) in &all[1..] {
                pb.line_to(x, y);
            }
            pb.close();
        };
        match self {
            Kind::Uav => poly(&[(0.0, -1.0), (0.16, -0.62), (0.98, 0.52), (0.98, 0.72), (0.22, 0.58), (0.0, 0.78)]),
            Kind::Recon => poly(&[
                (0.0, -1.0), (0.1, -0.85), (0.1, -0.35), (0.98, -0.25), (0.98, -0.05), (0.1, 0.05),
                (0.1, 0.62), (0.45, 0.72), (0.45, 0.9), (0.0, 0.9),
            ]),
            Kind::Missile => poly(&[
                (0.0, -1.0), (0.12, -0.78), (0.12, -0.08), (0.62, 0.08), (0.62, 0.2), (0.12, 0.16),
                (0.12, 0.62), (0.4, 0.86), (0.4, 0.98), (0.0, 0.9),
            ]),
            Kind::Ballistic => poly(&[
                (0.0, -1.0), (0.2, -0.62), (0.24, -0.3), (0.24, 0.42), (0.62, 0.78), (0.62, 1.0),
                (0.24, 0.86), (0.0, 0.86),
            ]),
            Kind::Mig31k => poly(&[
                (0.0, -1.0), (0.12, -0.6), (0.14, -0.2), (0.95, 0.25), (0.95, 0.42), (0.16, 0.3),
                (0.16, 0.58), (0.5, 0.82), (0.5, 0.98), (0.0, 0.9),
            ]),
            Kind::Kab => {
                poly(&[(0.0, 0.35), (0.62, 0.72), (0.62, 1.0), (0.0, 0.85)]);
                pb.push_oval(tiny_skia::Rect::from_xywh(-0.34, -1.0, 0.68, 1.5).unwrap());
            }
            Kind::Fpv => {
                pb.push_rect(tiny_skia::Rect::from_xywh(-0.22, -0.22, 0.44, 0.44).unwrap());
                for (x, y) in [(-0.62, -0.62), (0.62, -0.62), (-0.62, 0.62), (0.62, 0.62)] {
                    pb.push_circle(x, y, 0.36);
                }
            }
            Kind::Unknown => poly(&[(0.0, -0.95), (0.75, 0.0), (0.0, 0.95)]),
        }
        pb.finish().expect("glyph path")
    }
}
