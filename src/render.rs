//! Renders the status card: header (tracked region status), map with alerts + threats (whole
//! Ukraine, or the tracked oblast + neighbours when something is going on nearby), footer.

use crate::data::{alert_title, hromada_short, oblast_key, AreaAlert, Alerts, Threat};
use crate::geo::{haversine_km, View};
use crate::region::{Geo, Region};
use crate::glyph::Kind;
use crate::text::Text;
use chrono::{DateTime, Utc};
use chrono_tz::Europe::Kyiv;
use tiny_skia::{Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke, StrokeDash, Transform};

/// Anything closer than this to the tracked region is a warning.
pub const NEAR_KM: f64 = 100.0;
/// A target this close is a second, louder event.
pub const CLOSE_KM: f64 = 30.0;
/// Missiles / Kinzhals are reported from this far out.
pub const WATCH_KM: f64 = 300.0;
pub const HEADER: u32 = 200;
pub const FOOTER: u32 = 170;

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Level {
    Red,
    Yellow,
    Oblast,
    Clear,
}

impl Level {
    pub fn title(self) -> &'static str {
        match self {
            Level::Red => "Повітряна тривога",
            Level::Yellow => "Повітряна тривога · дрони",
            Level::Oblast => "Тривога в області",
            Level::Clear => "Тривоги немає",
        }
    }
    pub fn is_alert(self) -> bool {
        matches!(self, Level::Red | Level::Yellow)
    }
    pub fn emoji(self) -> &'static str {
        match self {
            Level::Red => "🔴",
            Level::Yellow => "🟠",
            Level::Oblast => "🟡",
            Level::Clear => "🟢",
        }
    }
    fn rgb(self) -> (u8, u8, u8) {
        match self {
            Level::Red => (255, 59, 48),
            Level::Yellow => (255, 149, 0),
            Level::Oblast => (255, 214, 10),
            Level::Clear => (52, 199, 89),
        }
    }
    fn on(self) -> (u8, u8, u8, u8) {
        match self {
            Level::Red => (255, 255, 255, 255),
            _ => (0, 0, 0, 255),
        }
    }
}

#[derive(Clone)]
pub struct Summary {
    pub level: Level,
    pub since: Option<DateTime<Utc>>,
    /// Active raions and hromadas of the tracked oblast.
    pub oblast_raions: Vec<String>,
    /// Open threats of the alerts that set the level, most dangerous first ("Балістика").
    pub threats: Vec<String>,
    /// Newest source wording of those threats.
    pub message: Option<String>,
    /// Non-air-raid alerts over the tracked region ("Загроза артобстрілу").
    pub other: Vec<String>,
    /// (distance km, kind) of every point threat within NEAR_KM, nearest first.
    pub near: Vec<(f64, Kind)>,
    /// (track id, distance km, kind) of every point threat within WATCH_KM, nearest first.
    pub tracks: Vec<(String, f64, Kind)>,
    pub now: DateTime<Utc>,
    pub label: String,
    /// Tracked oblast, short: "Львівська обл.", "м. Київ".
    pub oblast: String,
    /// The whole oblast is under alert (oblast-level entry, no per-raion list).
    pub whole_oblast: bool,
    /// Alerts in the tracked oblast or its neighbours, or targets nearby: draw the regional map.
    pub regional: bool,
}

impl Summary {
    pub fn make(alerts: &Alerts, threats: &[Threat], region: &Region, geo: &Geo) -> Summary {
        let home = geo.home(region);
        let ok = region.oblast_key();
        let rk = region.raion_key();
        let in_ob = |a: &&AreaAlert| oblast_key(&a.oblast) == ok;
        let in_oblast: Vec<&AreaAlert> = alerts.raions.iter().filter(in_ob).collect();
        let hromadas: Vec<&AreaAlert> = alerts.hromadas.iter().filter(in_ob).collect();
        let whole = alerts.oblasts.iter().find(|o| o.key == ok);
        // Whole oblast, or the tracked raion and its hromadas.
        let covers = |a: &AreaAlert| oblast_key(&a.oblast) == ok && rk.as_ref().is_none_or(|k| a.key == ok || a.key == *k);
        let here: Vec<&AreaAlert> = in_oblast.iter().chain(&hromadas).copied().chain(whole).filter(|a| covers(a)).collect();
        let (level, since) = if !here.is_empty() {
            let red = here.iter().any(|a| a.level != "yellow");
            (if red { Level::Red } else { Level::Yellow }, here.iter().filter_map(|a| a.since).min())
        } else if in_oblast.is_empty() && hromadas.is_empty() {
            (Level::Clear, None)
        } else {
            (Level::Oblast, in_oblast.iter().chain(&hromadas).filter_map(|r| r.since).min())
        };
        let mut oblast_raions: Vec<String> = in_oblast.iter().map(|r| r.name.clone()).collect();
        oblast_raions.sort_by_key(|n| crate::data::uk_key(n));
        let mut hs: Vec<String> = hromadas.iter().map(|h| hromada_short(&h.name)).collect();
        hs.sort_by_key(|n| crate::data::uk_key(n));
        oblast_raions.extend(hs);
        let mut open: Vec<_> = here.iter().flat_map(|a| &a.threats).collect();
        open.sort_by_key(|t| t.rank());
        let mut threats_l: Vec<String> = vec![];
        for t in &open {
            if !threats_l.iter().any(|l| l == t.label()) {
                threats_l.push(t.label().to_string());
            }
        }
        let message = open.iter().filter(|t| t.message.is_some()).max_by_key(|t| t.since).and_then(|t| t.message.clone());
        let mut other: Vec<String> = vec![];
        for a in alerts.other.iter().filter(|a| covers(a)) {
            if !other.iter().any(|o| o == alert_title(&a.kind)) {
                other.push(alert_title(&a.kind).to_string());
            }
        }
        let mut tracks: Vec<(String, f64, Kind)> = threats
            .iter()
            .filter(|t| t.is_point() && t.advisory != Some(true))
            .map(|t| (t.id.clone(), haversine_km(home, (t.lon, t.lat)), Kind::from(&t.kind)))
            .filter(|(_, d, _)| *d <= WATCH_KM)
            .collect();
        tracks.sort_by(|a, b| a.1.total_cmp(&b.1));
        let near: Vec<(f64, Kind)> = tracks.iter().filter(|(_, d, _)| *d <= NEAR_KM).map(|(_, d, k)| (*d, *k)).collect();
        let nb = geo.neighbours(&ok);
        let around = alerts.raions.iter().chain(&alerts.hromadas).any(|r| nb.contains(&oblast_key(&r.oblast)))
            || alerts.oblasts.iter().any(|o| nb.contains(&o.key));
        Summary {
            regional: level != Level::Clear || around || !near.is_empty(),
            level,
            since,
            oblast_raions,
            threats: if level.is_alert() { threats_l } else { vec![] },
            message: if level.is_alert() { message } else { None },
            other,
            near,
            tracks,
            now: Utc::now(),
            label: region.label(),
            oblast: region.oblast.replace(" область", " обл."),
            whole_oblast: whole.is_some(),
        }
    }

    /// Short caption: status, a warning only when something is close, update time.

    pub fn near_text(&self) -> String {
        match self.near.first() {
            None => format!("Цілей до {} км немає", NEAR_KM as i32),
            Some((d, _)) if self.near.len() == 1 => format!("⚠️ Ціль за {} км", *d as i32),
            Some((d, _)) => format!("⚠️ Цілей до {} км: {}, найближча {} км", NEAR_KM as i32, self.near.len(), *d as i32),
        }
    }

}

pub fn hm(t: DateTime<Utc>) -> String {
    t.with_timezone(&Kyiv).format("%H:%M").to_string()
}

fn paint(r: u8, g: u8, b: u8, a: u8) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(r, g, b, a);
    p.anti_alias = true;
    p
}

fn stroke(width: f32, dash: Option<(f32, f32)>) -> Stroke {
    Stroke {
        width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        dash: dash.and_then(|(on, off)| StrokeDash::new(vec![on, off], 0.0)),
        ..Stroke::default()
    }
}

fn ring_path(view: &View, ring: &[(f64, f64)]) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    let (x, y) = view.px(ring[0].0, ring[0].1);
    pb.move_to(x, y);
    for &(lon, lat) in &ring[1..] {
        let (x, y) = view.px(lon, lat);
        pb.line_to(x, y);
    }
    pb.close();
    pb.finish()
}

pub struct Renderer {
    pub text: Text,
    /// "Outside Ukraine" mask per view, built once.
    masks: std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tiny_skia::Mask>>>,
}

impl Renderer {
    /// Bake the static layer into a view's base map: everything outside Ukraine darkened and a
    /// bright band along the national border (the outer half of a thick stroke around all
    /// oblasts; the inner half is masked away, so internal borders stay thin).
    pub fn new(text: Text) -> Self {
        Renderer { text, masks: Default::default() }
    }

    /// Mask of everything outside Ukraine in view `v` (+ the path of all oblast outlines).
    fn outside(&self, v: &View, geo: &Geo) -> (std::sync::Arc<tiny_skia::Mask>, Option<tiny_skia::Path>) {
        let id = Transform::identity();
        let mut outside = tiny_skia::Mask::new(v.w, v.h).expect("mask");
        let mut all = PathBuilder::new();
        for rings in geo.oblasts.values() {
            let mut pb = PathBuilder::new();
            for ring in rings {
                let (x, y) = v.px(ring[0].0, ring[0].1);
                pb.move_to(x, y);
                for &(lon, lat) in &ring[1..] {
                    let (x, y) = v.px(lon, lat);
                    pb.line_to(x, y);
                }
                pb.close();
            }
            if let Some(p) = pb.finish() {
                outside.fill_path(&p, FillRule::EvenOdd, true, id);
                all.push_path(&p);
            }
        }
        outside.invert();
        let m = std::sync::Arc::new(outside);
        self.masks.lock().unwrap().insert(v.key(), m.clone());
        (m, all.finish())
    }

    fn outside_cached(&self, v: &View, geo: &Geo) -> std::sync::Arc<tiny_skia::Mask> {
        if let Some(m) = self.masks.lock().unwrap().get(&v.key()) {
            return m.clone();
        }
        self.outside(v, geo).0
    }

    pub fn decorate(&self, v: &View, base: &mut Pixmap, geo: &Geo) {
        let id = Transform::identity();
        let (outside, all) = self.outside(v, geo);
        let full = Rect::from_xywh(0.0, 0.0, v.w as f32, v.h as f32).unwrap();
        base.fill_rect(full, &paint(0, 0, 0, 175), id, Some(&outside));
        if let Some(p) = all {
            // Outside half only: ~5 px yellow band with a dark rim beyond it.
            base.stroke_path(&p, &paint(0, 0, 0, 230), &stroke(18.0, None), id, Some(&outside));
            base.stroke_path(&p, &paint(255, 214, 10, 255), &stroke(11.0, None), id, Some(&outside));
        }
    }

    /// `base` is the tile map of `v`. Regional views (zoom 8) get distance labels and the 100 km
    /// ring; the Ukraine view (zoom 7) keeps markers smaller.
    #[allow(clippy::too_many_arguments)]
    pub fn render(&self, v: &View, base: &Pixmap, geo: &Geo, region: &Region, alerts: &Alerts, threats: &[Threat], s: &Summary) -> Vec<u8> {
        let mut map = base.clone();
        let id = Transform::identity();
        let regional = s.regional;
        let home = geo.home(region);
        let scale = if regional { 1.0 } else { 0.72 };

        // 45° semi-transparent stripes over everything abroad, in the tracked region's status colour.
        {
            let outside = self.outside_cached(v, geo);
            let (cr, cg, cb) = s.level.rgb();
            let mut pb = PathBuilder::new();
            let (w, h) = (v.w as f32, v.h as f32);
            let mut x = 0.0;
            while x < w + h {
                pb.move_to(x, 0.0);
                pb.line_to(x - h, h);
                x += 44.0;
            }
            if let Some(p) = pb.finish() {
                let st = Stroke { width: 16.0, line_cap: LineCap::Butt, ..Stroke::default() };
                map.stroke_path(&p, &paint(cr, cg, cb, 60), &st, id, Some(&outside));
            }
        }

        // Active raions; a hromada alert paints its whole raion the same way (no hromada shapes).
        for r in alerts.raions.iter().chain(&alerts.hromadas) {
            let (cr, cg, cb) = if r.level == "yellow" { (255, 149, 0) } else { (255, 59, 48) };
            for ring in geo.raions.get(&r.key).into_iter().flatten() {
                if let Some(p) = ring_path(v, ring) {
                    map.fill_path(&p, &paint(cr, cg, cb, 105), FillRule::EvenOdd, id, None);
                    map.stroke_path(&p, &paint(cr, cg, cb, 255), &stroke(1.5 * scale + 0.5, None), id, None);
                }
            }
        }
        // Shelling, street fights, chemical, nuclear: purple.
        for o in &alerts.other {
            for ring in geo.raions.get(&o.key).or_else(|| geo.oblasts.get(&o.key)).into_iter().flatten() {
                if let Some(p) = ring_path(v, ring) {
                    map.fill_path(&p, &paint(175, 82, 222, 90), FillRule::EvenOdd, id, None);
                }
            }
        }
        // Whole-oblast alerts (no per-raion data).
        for o in &alerts.oblasts {
            let (cr, cg, cb) = if o.level == "yellow" { (255, 149, 0) } else { (255, 59, 48) };
            for ring in geo.oblasts.get(&o.key).into_iter().flatten() {
                if let Some(p) = ring_path(v, ring) {
                    map.fill_path(&p, &paint(cr, cg, cb, 80), FillRule::EvenOdd, id, None);
                }
            }
        }
        // Oblast borders.
        let tracked = region.oblast_key();
        for (key, rings) in &geo.oblasts {
            if *key == tracked {
                continue;
            }
            for ring in rings {
                if let Some(p) = ring_path(v, ring) {
                    map.stroke_path(&p, &paint(255, 255, 255, 110), &stroke(1.4 * scale, None), id, None);
                }
            }
        }
        // Tracked region: glowing cyan oblast outline, white tracked raion on top.
        for ring in geo.oblasts.get(&tracked).into_iter().flatten() {
            if let Some(p) = ring_path(v, ring) {
                map.fill_path(&p, &paint(80, 200, 255, 28), FillRule::EvenOdd, id, None);
                map.stroke_path(&p, &paint(0, 0, 0, 150), &stroke(10.0 * scale, None), id, None);
                map.stroke_path(&p, &paint(80, 200, 255, 255), &stroke(5.0 * scale, None), id, None);
            }
        }
        if let Some(rk) = region.raion_key() {
            for ring in geo.raions.get(&rk).into_iter().flatten() {
                if let Some(p) = ring_path(v, ring) {
                    map.stroke_path(&p, &paint(255, 255, 255, 255), &stroke(3.0 * scale, Some((10.0, 6.0))), id, None);
                }
            }
        }
        // NEAR_KM ring (regional view only).
        let (lx, ly) = v.px(home.0, home.1);
        if regional {
            let r_px = v.px_per_km(home.1) * NEAR_KM as f32;
            if let Some(c) = PathBuilder::from_circle(lx, ly, r_px) {
                map.fill_path(&c, &paint(255, 255, 255, 14), FillRule::Winding, id, None);
                map.stroke_path(&c, &paint(255, 255, 255, 220), &stroke(2.0, Some((10.0, 8.0))), id, None);
            }
        }

        // Threats: trail, silhouette rotated to course, distance label when close.
        let g = 18.0 * scale;
        for t in threats.iter().filter(|t| t.is_point()) {
            let (x, y) = v.px(t.lon, t.lat);
            if !v.contains(x, y, 30.0) {
                continue;
            }
            let k = Kind::from(&t.kind);
            let (cr, cg, cb) = if t.advisory == Some(true) { (158, 158, 158) } else { k.rgb() };
            if let Some(trail) = t.trail.as_ref().filter(|tr| tr.len() > 1) {
                let mut pb = PathBuilder::new();
                let (x0, y0) = v.px(trail[0].lon, trail[0].lat);
                pb.move_to(x0, y0);
                for p in &trail[1..] {
                    let (px, py) = v.px(p.lon, p.lat);
                    pb.line_to(px, py);
                }
                if let Some(p) = pb.finish() {
                    map.stroke_path(&p, &paint(cr, cg, cb, 220), &stroke(2.5 * scale, Some((6.0, 4.0))), id, None);
                }
            }
            let rot = if k.shows_heading() { t.course().unwrap_or(0.0) as f32 } else { 0.0 };
            let tr = Transform::from_scale(g, g).post_rotate(rot).post_translate(x, y);
            let shadow = Transform::from_scale(g, g).post_rotate(rot).post_translate(x + 2.0, y + 2.5);
            let path = k.path();
            map.fill_path(&path, &paint(0, 0, 0, 150), FillRule::Winding, shadow, None);
            map.fill_path(&path, &paint(cr, cg, cb, 255), FillRule::Winding, tr, None);
            map.stroke_path(&path, &paint(255, 255, 255, 255), &stroke(2.5 / g, None), tr, None);
            let km = haversine_km(home, (t.lon, t.lat));
            if regional && km <= 160.0 {
                let label = format!("{} км", km as i32);
                let w = self.text.width(&label, 31.0);
                let bx = x + 24.0;
                if let Some(r) = Rect::from_xywh(bx - 6.0, y - 25.0, w + 12.0, 36.0) {
                    map.fill_rect(r, &paint(0, 0, 0, 190), id, None);
                }
                self.text.draw(&mut map, &label, bx, y + 3.0, 31.0, (255, 255, 255, 255));
            }
        }

        // Home dot + label.
        if let Some(c) = PathBuilder::from_circle(lx, ly, 9.0 * scale + 2.0) {
            map.fill_path(&c, &paint(10, 132, 255, 255), FillRule::Winding, id, None);
            map.stroke_path(&c, &paint(255, 255, 255, 255), &stroke(3.5, None), id, None);
        }
        let fs = if regional { 38.0 } else { 32.0 };
        let lw = self.text.width(&s.label, fs);
        if let Some(r) = Rect::from_xywh(lx + 14.0, ly - 14.0 - fs, lw + 12.0, fs + 12.0) {
            map.fill_rect(r, &paint(0, 0, 0, 150), id, None);
        }
        self.text.draw(&mut map, &s.label, lx + 20.0, ly - 14.0, fs, (255, 255, 255, 255));

        // Warning banner on the map.
        if !s.near.is_empty() {
            let msg = s.near_text().replace("⚠️ ", "⚠ ");
            let w = self.text.width(&msg, 46.0);
            if let Some(r) = Rect::from_xywh(14.0, 14.0, w + 40.0, 74.0) {
                map.fill_rect(r, &paint(255, 59, 48, 240), id, None);
            }
            self.text.draw(&mut map, &msg, 34.0, 67.0, 46.0, (255, 255, 255, 255));
        }
        // View badge.
        let badge = if regional { "Область і сусіди" } else { "Вся Україна" };
        let bw = self.text.width(badge, 40.0);
        if let Some(r) = Rect::from_xywh(v.w as f32 - bw - 50.0, v.h as f32 - 78.0, bw + 34.0, 58.0) {
            map.fill_rect(r, &paint(0, 0, 0, 180), id, None);
        }
        self.text.draw(&mut map, badge, v.w as f32 - bw - 33.0, v.h as f32 - 34.0, 40.0, (255, 255, 255, 240));

        // Compose header + map + footer.
        let (w, h) = (v.w, HEADER + v.h + FOOTER);
        let mut card = Pixmap::new(w, h).expect("card");
        card.fill(Color::from_rgba8(18, 18, 20, 255));
        if regional {
            let (cr, cg, cb) = s.level.rgb();
            card.fill_rect(Rect::from_xywh(0.0, 0.0, w as f32, HEADER as f32).unwrap(), &paint(cr, cg, cb, 255), id, None);
            let on = s.level.on();
            self.text.draw(&mut card, &format!("{} · {}", s.label, s.level.title()), 28.0, 86.0, 68.0, on);
            // Threats before the (possibly long, clipped) list of raions and hromadas.
            let mut sub = s.since.map(|t| format!("з {}", hm(t))).unwrap_or_default();
            if !s.threats.is_empty() {
                if !sub.is_empty() {
                    sub += " · ";
                }
                sub += &s.threats.join(", ");
            }
            if !s.oblast_raions.is_empty() {
                if !sub.is_empty() {
                    sub += " · ";
                }
                let short: Vec<String> = s.oblast_raions.iter().map(|r| r.replace(" район", "")).collect();
                sub += &short.join(", ");
            }
            self.text.draw(&mut card, &sub, 28.0, 162.0, 44.0, on);
        } else {
            // Whole Ukraine: neutral header with the number of oblasts under alert.
            let mut oblasts: std::collections::BTreeSet<&str> =
                alerts.raions.iter().chain(&alerts.hromadas).map(|r| r.oblast.as_str()).collect();
            oblasts.extend(alerts.oblasts.iter().map(|o| o.name.as_str()));
            let n = oblasts.len();
            card.fill_rect(Rect::from_xywh(0.0, 0.0, w as f32, HEADER as f32).unwrap(), &paint(38, 42, 52, 255), id, None);
            let title = match n {
                0 => "Україна · тривог немає".to_string(),
                1 => "Україна · тривога в 1 області".to_string(),
                n => format!("Україна · тривога в {n} областях"),
            };
            self.text.draw(&mut card, &title, 28.0, 86.0, 68.0, (255, 255, 255, 255));
            self.text.draw(&mut card, &format!("{}: {}", s.label, s.level.title().to_lowercase()), 28.0, 162.0, 44.0, (200, 204, 214, 255));
        }
        card.draw_pixmap(0, HEADER as i32, map.as_ref(), &PixmapPaint::default(), id, None);

        // Footer, only what is on the map. Row 1: NEPTUN target types with counts. Row 2: area colours.
        let fy = (HEADER + v.h) as f32;
        let counts: Vec<(Kind, usize)> = Kind::ALL
            .iter()
            .map(|&k| {
                let n = threats
                    .iter()
                    .filter(|t| t.is_point() && Kind::from(&t.kind) == k && {
                        let (x, y) = v.px(t.lon, t.lat);
                        v.contains(x, y, 0.0)
                    })
                    .count();
                (k, n)
            })
            .filter(|(_, n)| *n > 0)
            .collect();
        let white = (255, 255, 255, 255);
        let mut x = 28.0;
        for (k, n) in counts {
            if x > w as f32 - 260.0 {
                break;
            }
            let tr = Transform::from_scale(15.0, 15.0).post_translate(x + 15.0, fy + 46.0);
            card.fill_path(&k.path(), &k.color().to_paint(), FillRule::Winding, tr, None);
            card.stroke_path(&k.path(), &paint(255, 255, 255, 255), &stroke(0.13, None), tr, None);
            x = self.text.draw(&mut card, &format!("{} — {n}", k.short()), x + 40.0, fy + 58.0, 34.0, white) + 36.0;
        }
        let red = alerts.raions.iter().chain(&alerts.oblasts).chain(&alerts.hromadas).any(|a| a.level != "yellow");
        let yellow = alerts.raions.iter().chain(&alerts.oblasts).chain(&alerts.hromadas).any(|a| a.level == "yellow");
        let swatches = [
            (red, (255, 59, 48, 200), "тривога"),
            (yellow, (255, 149, 0, 200), "дрони"),
            (!alerts.other.is_empty(), (175, 82, 222, 200), "обстріл, бої"),
        ];
        let (ry, fs) = (fy + 118.0, 28.0);
        let mut x = 28.0;
        for (_, (r, g, b, a), label) in swatches.iter().filter(|s| s.0) {
            if let Some(rect) = Rect::from_xywh(x, ry - 22.0, 24.0, 24.0) {
                card.fill_rect(rect, &paint(*r, *g, *b, *a), id, None);
            }
            x = self.text.draw(&mut card, label, x + 32.0, ry, fs, white) + 30.0;
        }
        let src = "alerts.in.ua · NEPTUN · © OpenStreetMap";
        let sw = self.text.width(src, 20.0);
        self.text.draw(&mut card, src, w as f32 - sw - 20.0, h as f32 - 12.0, 20.0, (150, 150, 158, 255));
        card.encode_png().expect("png")
    }
}

trait ToPaint {
    fn to_paint(self) -> Paint<'static>;
}
impl ToPaint for Color {
    fn to_paint(self) -> Paint<'static> {
        let mut p = Paint::default();
        p.set_color(self);
        p.anti_alias = true;
        p
    }
}
