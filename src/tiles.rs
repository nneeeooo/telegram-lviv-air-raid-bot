//! Base map from OpenStreetMap standard tiles (© OpenStreetMap contributors), darkened.
//! Per the OSM tile policy: identifying User-Agent, tiles fetched once and cached on disk
//! (~40 tiles at zoom 8), never re-downloaded while the bot runs.

use crate::geo::{View, TILE};
use anyhow::{Context, Result};
use std::path::Path;
use tiny_skia::{Color, Paint, Pixmap, PixmapPaint, Rect, Transform};

const URL: &str = "https://tile.openstreetmap.org";

pub async fn base_map(http: &reqwest::Client, cache: &Path, view: &View) -> Result<Pixmap> {
    let dir = cache.join("tiles");
    std::fs::create_dir_all(&dir)?;
    let mut map = Pixmap::new(view.w, view.h).context("pixmap")?;
    let (tx0, ty0) = ((view.x0 / TILE).floor() as i64, (view.y0 / TILE).floor() as i64);
    let tx1 = ((view.x0 + view.w as f64) / TILE).floor() as i64;
    let ty1 = ((view.y0 + view.h as f64) / TILE).floor() as i64;
    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            let file = dir.join(format!("{}_{}_{}.png", view.z, tx, ty));
            let bytes = match std::fs::read(&file) {
                Ok(b) => b,
                Err(_) => {
                    let url = format!("{URL}/{}/{}/{}.png", view.z, tx, ty);
                    let b = http.get(&url).send().await?.error_for_status()?.bytes().await?.to_vec();
                    std::fs::write(&file, &b)?;
                    b
                }
            };
            let tile = Pixmap::decode_png(&bytes).with_context(|| format!("tile {tx},{ty}"))?;
            let x = (tx as f64 * TILE - view.x0).round() as i32;
            let y = (ty as f64 * TILE - view.y0).round() as i32;
            map.draw_pixmap(x, y, tile.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
        }
    }
    // Darken + desaturate-ish so red / orange overlays pop.
    let mut shade = Paint::default();
    shade.set_color(Color::from_rgba8(12, 14, 22, 175));
    map.fill_rect(Rect::from_xywh(0.0, 0.0, view.w as f32, view.h as f32).unwrap(), &shade, Transform::identity(), None);
    Ok(map)
}
