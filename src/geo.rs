//! Web Mercator projection for a fixed map view (lon/lat -> image pixels).

pub const TILE: f64 = 256.0;

/// Lviv city centre.
pub const LVIV: (f64, f64) = (24.0297, 49.8397);

/// Whole Ukraine (mainland + Crimea).
pub const UKRAINE: (f64, f64, f64, f64) = (21.6, 44.2, 40.6, 52.5); // west, south, east, north

pub fn world_px(lon: f64, lat: f64, z: u32) -> (f64, f64) {
    let n = TILE * 2f64.powi(z as i32);
    let x = (lon + 180.0) / 360.0 * n;
    let r = lat.to_radians();
    let y = (1.0 - (r.tan() + 1.0 / r.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
    (x, y)
}

#[derive(Clone, Copy)]
pub struct View {
    pub z: u32,
    /// World-pixel coordinates of the image's top-left corner.
    pub x0: f64,
    pub y0: f64,
    pub w: u32,
    pub h: u32,
}

impl View {
    /// View of a lon/lat box at zoom `z`, widened or heightened so that the whole card (map +
    /// `extra_h` of header and footer) is square: Telegram iOS pads tall photos with blurred sides.
    pub fn fit(bbox: (f64, f64, f64, f64), z: u32, extra_h: u32) -> Self {
        let (w, s, e, n) = bbox;
        let (x0, y0) = world_px(w, n, z);
        let (x1, y1) = world_px(e, s, z);
        let (bw, bh) = (x1 - x0, y1 - y0);
        let side = bw.max(bh + extra_h as f64);
        let (mw, mh) = (side, side - extra_h as f64);
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        View { z, x0: cx - mw / 2.0, y0: cy - mh / 2.0, w: mw.round() as u32, h: mh.round() as u32 }
    }

    /// Stable id for caching the base map of this view.
    pub fn key(&self) -> String {
        format!("{}_{}_{}_{}_{}", self.z, self.x0.round(), self.y0.round(), self.w, self.h)
    }

    pub fn px(&self, lon: f64, lat: f64) -> (f32, f32) {
        let (x, y) = world_px(lon, lat, self.z);
        ((x - self.x0) as f32, (y - self.y0) as f32)
    }

    /// Pixels per km at the given latitude.
    pub fn px_per_km(&self, lat: f64) -> f32 {
        let (_, y0) = world_px(LVIV.0, lat, self.z);
        let (_, y1) = world_px(LVIV.0, lat + 1.0 / 111.2, self.z);
        (y0 - y1).abs() as f32
    }

    pub fn contains(&self, x: f32, y: f32, margin: f32) -> bool {
        x > -margin && y > -margin && x < self.w as f32 + margin && y < self.h as f32 + margin
    }
}

pub fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (lon1, lat1) = (a.0.to_radians(), a.1.to_radians());
    let (lon2, lat2) = (b.0.to_radians(), b.1.to_radians());
    let h = ((lat2 - lat1) / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * ((lon2 - lon1) / 2.0).sin().powi(2);
    2.0 * 6371.0 * h.sqrt().asin()
}
