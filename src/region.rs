//! Tracked region (one oblast or one raion per chat) and the geometry around it: home point for
//! distances, oblast neighbours (from shared borders), the box for the regional map.

use crate::data::{oblast_key, raion_key, Shapes};
use crate::geo::{View, LVIV};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Region {
    /// As the alert source names it: "Львівська область", "м. Київ".
    pub oblast: String,
    /// "Львівський район"; None = the whole oblast.
    pub raion: Option<String>,
}

impl Default for Region {
    fn default() -> Self {
        Region { oblast: "Львівська область".into(), raion: Some("Львівський район".into()) }
    }
}

impl Region {
    /// Short label for headers and buttons: "Львівський р-н", "Одеська обл.".
    pub fn label(&self) -> String {
        match &self.raion {
            Some(r) => r.replace(" район", " р-н"),
            None => self.oblast.replace(" область", " обл."),
        }
    }

    pub fn oblast_key(&self) -> String {
        oblast_key(&self.oblast)
    }

    pub fn raion_key(&self) -> Option<String> {
        self.raion.as_deref().map(raion_key)
    }
}

type BBox = (f64, f64, f64, f64); // west, south, east, north

fn bbox(rings: &[Vec<(f64, f64)>]) -> BBox {
    let mut b = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for &(x, y) in rings.iter().flatten() {
        b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
    }
    b
}

/// Area centroid of the largest ring.
fn centroid(rings: &[Vec<(f64, f64)>]) -> Option<(f64, f64)> {
    let area = |r: &Vec<(f64, f64)>| -> f64 {
        r.windows(2).map(|w| w[0].0 * w[1].1 - w[1].0 * w[0].1).sum::<f64>() / 2.0
    };
    let ring = rings.iter().max_by(|a, b| area(a).abs().total_cmp(&area(b).abs()))?;
    let a = area(ring);
    if a.abs() < 1e-12 {
        return None;
    }
    let (mut cx, mut cy) = (0.0, 0.0);
    for w in ring.windows(2) {
        let f = w[0].0 * w[1].1 - w[1].0 * w[0].1;
        cx += (w[0].0 + w[1].0) * f;
        cy += (w[0].1 + w[1].1) * f;
    }
    Some((cx / (6.0 * a), cy / (6.0 * a)))
}

pub struct Geo {
    pub raions: Shapes,
    pub oblasts: Shapes,
    boxes: HashMap<String, BBox>,
    neighbours: HashMap<String, BTreeSet<String>>,
}

impl Geo {
    pub fn new(raions: Shapes, oblasts: Shapes) -> Geo {
        let boxes: HashMap<String, BBox> = oblasts.iter().map(|(k, r)| (k.clone(), bbox(r))).collect();
        // Two oblasts are neighbours when some border vertices are within ~5 km.
        const TOUCH: f64 = 0.05;
        let mut neighbours: HashMap<String, BTreeSet<String>> = HashMap::new();
        let keys: Vec<&String> = oblasts.keys().collect();
        for (i, a) in keys.iter().enumerate() {
            for b in &keys[i + 1..] {
                let (ba, bb) = (boxes[*a], boxes[*b]);
                if ba.0 > bb.2 + TOUCH || bb.0 > ba.2 + TOUCH || ba.1 > bb.3 + TOUCH || bb.1 > ba.3 + TOUCH {
                    continue;
                }
                let pa: Vec<(f64, f64)> = oblasts[*a].iter().flatten().step_by(3).copied().collect();
                let pb: Vec<(f64, f64)> = oblasts[*b].iter().flatten().step_by(3).copied().collect();
                let touch = pa.iter().any(|p| pb.iter().any(|q| (p.0 - q.0).abs() < TOUCH && (p.1 - q.1).abs() < TOUCH));
                if touch {
                    neighbours.entry((*a).clone()).or_default().insert((*b).clone());
                    neighbours.entry((*b).clone()).or_default().insert((*a).clone());
                }
            }
        }
        Geo { raions, oblasts, boxes, neighbours }
    }

    /// Point distances are measured from: Lviv city for Lviv raion, else the region's centroid.
    pub fn home(&self, r: &Region) -> (f64, f64) {
        if r.raion.as_deref() == Some("Львівський район") {
            return LVIV;
        }
        let shape = match r.raion_key() {
            Some(k) => self.raions.get(&k),
            None => self.oblasts.get(&r.oblast_key()),
        };
        shape.and_then(|s| centroid(s)).unwrap_or(LVIV)
    }

    pub fn neighbours(&self, oblast_key: &str) -> BTreeSet<String> {
        self.neighbours.get(oblast_key).cloned().unwrap_or_default()
    }

    /// The tracked oblast centred, plus a margin that shows its neighbours.
    pub fn regional_view(&self, r: &Region, extra_h: u32) -> View {
        let (w, s, e, n) = self.boxes.get(&r.oblast_key()).copied().unwrap_or((22.6, 48.7, 25.4, 50.8));
        let bx = (w - 1.3, s - 0.9, e + 1.3, n + 0.9);
        let v = View::fit(bx, 8, extra_h);
        if v.w <= 1900 { v } else { View::fit(bx, 7, extra_h) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centroid_of_square() {
        let sq = vec![vec![(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0), (0.0, 0.0)]];
        let (x, y) = centroid(&sq).unwrap();
        assert!((x - 1.0).abs() < 1e-9 && (y - 1.0).abs() < 1e-9);
    }

    #[test]
    fn labels() {
        assert_eq!(Region::default().label(), "Львівський р-н");
        assert_eq!(Region { oblast: "Одеська область".into(), raion: None }.label(), "Одеська обл.");
    }
}
