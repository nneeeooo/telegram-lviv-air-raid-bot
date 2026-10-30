//! Data sources: alerts from alerts.in.ua (oblast / raion / hromada, alert types, open threats;
//! token in ALERTS_IN_UA_TOKEN), threat positions from NEPTUN, raion / oblast borders from NEPTUN
//! GeoJSON (cached on disk).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

pub const ALERTS_IN_UA: &str = "https://api.alerts.in.ua";
pub const NEPTUN: &str = "https://neptun.in.ua";

#[derive(Debug, Clone)]
pub struct AreaAlert {
    /// Shape key: oblasts.geojson for whole-oblast alerts, raions.geojson otherwise (a hromada's raion).
    pub key: String,
    pub name: String,
    /// Catalog oblast name ("Львівська область").
    pub oblast: String,
    /// alerts.in.ua alert_type: air_raid, artillery_shelling, urban_fights, chemical, nuclear.
    pub kind: String,
    pub level: String, // "red" | "yellow"
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// Open threats, most dangerous first.
    pub threats: Vec<AlertThreat>,
}

#[derive(Debug, Clone)]
pub struct AlertThreat {
    pub kind: String,
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    pub message: Option<String>,
}

impl AlertThreat {
    /// Most dangerous first.
    const ORDER: &[&str] = &[
        "ballistic_missiles", "mig31k_departure", "cruise_missiles", "unspecified_missiles",
        "strategic_aircraft_activity", "guided_aerial_bombs", "tactic_aircraft_activity", "drones", "air_defense",
    ];
    pub fn rank(&self) -> usize {
        Self::ORDER.iter().position(|k| *k == self.kind).unwrap_or(Self::ORDER.len())
    }
    pub fn label(&self) -> &'static str {
        match self.kind.as_str() {
            "ballistic_missiles" => "Балістика",
            "mig31k_departure" => "МіГ-31К",
            "cruise_missiles" => "Крилаті ракети",
            "unspecified_missiles" => "Ракети",
            "strategic_aircraft_activity" => "Стратегічна авіація",
            "guided_aerial_bombs" => "КАБи",
            "tactic_aircraft_activity" => "Тактична авіація",
            "drones" => "Дрони",
            "air_defense" => "Робота ППО",
            _ => "Загроза",
        }
    }
}

/// Title of a non-air-raid alert type.
pub fn alert_title(kind: &str) -> &'static str {
    match kind {
        "artillery_shelling" => "Загроза артобстрілу",
        "urban_fights" => "Вуличні бої",
        "chemical" => "Хімічна загроза",
        "nuclear" => "Радіаційна загроза",
        _ => "Тривога",
    }
}

#[derive(Debug, Clone, Default)]
pub struct Alerts {
    /// Air raid alerts by area size; `other` holds every non-air-raid alert.
    pub raions: Vec<AreaAlert>,
    pub oblasts: Vec<AreaAlert>,
    pub hromadas: Vec<AreaAlert>,
    pub other: Vec<AreaAlert>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrailPoint {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Velocity {
    #[serde(rename = "bearingDeg")]
    pub bearing_deg: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Threat {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub lat: f64,
    pub lon: f64,
    pub heading: Option<f64>,
    pub status: Option<String>,
    pub advisory: Option<bool>,
    #[serde(rename = "areaOnly")]
    pub area_only: Option<bool>,
    pub velocity: Option<Velocity>,
    pub trail: Option<Vec<TrailPoint>>,
}

impl Threat {
    /// Drawable as a point: active and not just a region centroid.
    pub fn is_point(&self) -> bool {
        self.area_only != Some(true) && self.status.as_deref().map_or(true, |s| s == "active")
    }
    pub fn course(&self) -> Option<f64> {
        self.heading.or(self.velocity.as_ref().and_then(|v| v.bearing_deg))
    }
}

pub fn parse_time(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&chrono::Utc))
}

/// "Львівський район" -> "львівський", the key used by NEPTUN's raions.geojson.
pub fn raion_key(name: &str) -> String {
    let k = name.to_lowercase().replace(" район", "").trim().to_string();
    // Raions renamed after the NEPTUN GeoJSON was made: map new names to the old shape keys.
    match k.as_str() {
        "шептицький" => "червоноградський".into(),
        "звягельський" => "новоград-волинський".into(),
        "володимирський" => "володимир-волинський".into(),
        "самарівський" => "новомосковський".into(),
        "сіверськодонецький" => "сєвєродонецький".into(),
        "берестинський" => "красноградський".into(),
        _ => k,
    }
}

/// "Львівська область" -> "львівська", "АР Крим" / "м. Севастополь" -> the oblasts.geojson keys.
pub fn oblast_key(name: &str) -> String {
    match name {
        "АР Крим" => "автономна республіка крим".into(),
        n => match n.to_lowercase().replace(" область", "").trim() {
            "м. севастополь" => "севастополь".into(),
            k => k.to_string(),
        },
    }
}

/// Catalog oblast for any spelling alerts.in.ua (or an old session) uses.
pub fn catalog_oblast(name: &str) -> Option<&'static crate::catalog::Oblast> {
    let k = oblast_key(name);
    crate::catalog::CATALOG.iter().find(|o| oblast_key(o.name) == k)
}

/// "м. Львів та Львівська територіальна громада" -> "м. Львів"; "Оваднівська територіальна громада" -> "Оваднівська гр.".
pub fn hromada_short(name: &str) -> String {
    match name.split_once(" та ") {
        Some((city, _)) if name.starts_with("м. ") => city.to_string(),
        _ => name.replace(" територіальна громада", " гр."),
    }
}

#[derive(Deserialize)]
struct AThreat {
    threat_type: String,
    started_at: Option<String>,
    source_message: Option<String>,
}

#[derive(Deserialize)]
struct AItem {
    location_title: String,
    location_type: String,
    location_oblast: Option<String>,
    location_raion: Option<String>,
    alert_type: String,
    alert_level: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
    threats: Option<Vec<AThreat>>,
}

#[derive(Deserialize)]
struct ARoot {
    alerts: Vec<AItem>,
}

/// Last-Modified + parsed answer, for If-Modified-Since (304 = unchanged, cheaper for the API).
static LAST: Mutex<Option<(String, Alerts)>> = Mutex::new(None);

/// Active alerts. Limits: ~10 requests/min per IP shared with the desktop app, so INTERVAL_SECS >= 10.
pub async fn fetch_alerts(http: &reqwest::Client) -> Result<Alerts> {
    let token = std::env::var("ALERTS_IN_UA_TOKEN").unwrap_or_default();
    anyhow::ensure!(!token.is_empty(), "ALERTS_IN_UA_TOKEN is not set (.env)");
    let cached = LAST.lock().unwrap().clone();
    let mut req = http.get(format!("{ALERTS_IN_UA}/v1/alerts/active.json")).bearer_auth(&token);
    if let Some((m, _)) = &cached {
        req = req.header("If-Modified-Since", m);
    }
    // The token travels in a header, so reqwest errors (which print the URL) cannot leak it.
    let resp = req.send().await?;
    match resp.status().as_u16() {
        304 => return cached.map(|(_, a)| a).context("304 without a cached answer"),
        401 => anyhow::bail!("alerts.in.ua: token rejected (401)"),
        429 => anyhow::bail!("alerts.in.ua: too many requests (429)"),
        _ => {}
    }
    let resp = resp.error_for_status()?;
    let modified = resp.headers().get("last-modified").and_then(|v| v.to_str().ok()).map(String::from);
    let root: ARoot = resp.json().await?;
    let out = parse_alerts(root.alerts);
    *LAST.lock().unwrap() = modified.map(|m| (m, out.clone()));
    Ok(out)
}

fn parse_alerts(items: Vec<AItem>) -> Alerts {
    let mut out = Alerts::default();
    for i in items.into_iter().filter(|i| i.finished_at.is_none()) {
        let oblast = i
            .location_oblast
            .as_deref()
            .and_then(catalog_oblast)
            .or_else(|| catalog_oblast(&i.location_title))
            .map(|o| o.name.to_string())
            .unwrap_or_else(|| i.location_oblast.clone().unwrap_or_default());
        // Kyiv and Sevastopol come as "city" but are oblast-level areas here.
        let whole = i.location_type == "oblast" || (i.location_type == "city" && catalog_oblast(&i.location_title).is_some());
        let mut threats: Vec<AlertThreat> = i
            .threats
            .unwrap_or_default()
            .into_iter()
            .map(|t| AlertThreat {
                kind: t.threat_type,
                since: t.started_at.as_deref().and_then(parse_time),
                message: t.source_message.filter(|m| !m.is_empty()),
            })
            .collect();
        threats.sort_by_key(|t| t.rank());
        let a = AreaAlert {
            key: if whole {
                oblast_key(&oblast)
            } else {
                let city = crate::catalog::city_raion(&i.location_title);
                raion_key(i.location_raion.as_deref().or(city).unwrap_or(&i.location_title))
            },
            name: i.location_title.clone(),
            oblast,
            kind: i.alert_type.clone(),
            level: i.alert_level.unwrap_or_else(|| "red".into()),
            since: i.started_at.as_deref().and_then(parse_time),
            threats,
        };
        if i.alert_type != "air_raid" {
            out.other.push(a);
        } else if whole {
            out.oblasts.push(a);
        } else if i.location_type == "raion" {
            out.raions.push(a);
        } else {
            out.hromadas.push(a);
        }
    }
    out
}

/// Ukrainian alphabetical sort key (byte order puts "І" before "Б").
pub fn uk_key(s: &str) -> Vec<u32> {
    const ABC: &str = "абвгґдеєжзиіїйклмнопрстуфхцчшщьюя";
    s.to_lowercase()
        .chars()
        .map(|c| ABC.chars().position(|a| a == c).map(|p| p as u32 + 1000).unwrap_or(c as u32 + 2000))
        .collect()
}

#[derive(Deserialize)]
struct Threats {
    threats: Vec<Threat>,
}

pub async fn fetch_threats(http: &reqwest::Client) -> Result<Vec<Threat>> {
    let t: Threats = http
        .get(format!("{NEPTUN}/api/v1/threats"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(t.threats)
}

/// Rings (lon, lat) per feature key.
pub type Shapes = HashMap<String, Vec<Vec<(f64, f64)>>>;

/// Loads a NEPTUN GeoJSON (raions / oblasts), downloading it once into `cache`.
pub async fn load_shapes(http: &reqwest::Client, cache: &Path, file: &str) -> Result<Shapes> {
    let path = cache.join(file);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => {
            let b = http.get(format!("{NEPTUN}/{file}")).send().await?.error_for_status()?.bytes().await?.to_vec();
            std::fs::write(&path, &b)?;
            b
        }
    };
    let v: serde_json::Value = serde_json::from_slice(&bytes).context("geojson")?;
    let mut out = Shapes::new();
    for f in v["features"].as_array().into_iter().flatten() {
        let Some(key) = f["properties"]["key"].as_str() else { continue };
        let g = &f["geometry"];
        let polys: Vec<&serde_json::Value> = match g["type"].as_str() {
            Some("Polygon") => vec![&g["coordinates"]],
            Some("MultiPolygon") => g["coordinates"].as_array().map(|a| a.iter().collect()).unwrap_or_default(),
            _ => continue,
        };
        for poly in polys {
            for ring in poly.as_array().into_iter().flatten() {
                let pts: Vec<(f64, f64)> = ring
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| Some((p[0].as_f64()?, p[1].as_f64()?)))
                    .collect();
                if pts.len() > 2 {
                    out.entry(key.to_string()).or_default().push(pts);
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_active_alerts() {
        let json = r#"{"alerts":[
          {"id":1,"location_title":"Львівська область","location_type":"oblast","location_oblast":"Львівська область","location_raion":null,
           "alert_type":"air_raid","alert_level":"red","started_at":"2026-10-22T10:00:00.000Z","finished_at":null,
           "threats":[{"threat_type":"drones","level":"yellow","started_at":"2026-10-22T10:00:00.000Z"},
                      {"threat_type":"ballistic_missiles","level":"red","started_at":"2026-10-22T10:05:00.000Z","source_message":"Загроза балістики"}]},
          {"id":2,"location_title":"Шептицький район","location_type":"raion","location_oblast":"Львівська область","location_raion":"Шептицький район",
           "alert_type":"air_raid","alert_level":"yellow","started_at":"2026-10-22T10:00:00Z","finished_at":null},
          {"id":3,"location_title":"м. Нікополь та Нікопольська територіальна громада","location_type":"hromada","location_oblast":"Дніпропетровська область",
           "location_raion":"Нікопольський район","alert_type":"artillery_shelling","started_at":"2026-10-22T09:00:00Z","finished_at":null},
          {"id":4,"location_title":"м. Київ","location_type":"city","location_oblast":"м. Київ","location_raion":null,
           "alert_type":"air_raid","started_at":"2026-10-22T09:00:00Z","finished_at":null},
          {"id":5,"location_title":"Оваднівська територіальна громада","location_type":"hromada","location_oblast":"Волинська область",
           "location_raion":"Володимирський район","alert_type":"air_raid","alert_level":"red","started_at":"2026-10-22T09:00:00Z","finished_at":null}
        ]}"#;
        let a = parse_alerts(serde_json::from_str::<ARoot>(json).unwrap().alerts);
        assert_eq!(a.oblasts.len(), 2);
        assert_eq!(a.oblasts[0].key, "львівська");
        assert_eq!(a.oblasts[0].threats[0].label(), "Балістика");
        assert_eq!(a.oblasts[1].key, "м. київ");
        assert_eq!(a.raions[0].key, "червоноградський");
        assert_eq!(a.hromadas[0].key, "володимир-волинський");
        assert_eq!(hromada_short(&a.hromadas[0].name), "Оваднівська гр.");
        assert_eq!(a.other[0].key, "нікопольський");
        assert_eq!(alert_title(&a.other[0].kind), "Загроза артобстрілу");
        assert_eq!(hromada_short(&a.other[0].name), "м. Нікополь");
        assert_eq!(catalog_oblast("АР Крим").map(|o| o.uid), Some(29));
        let city = r#"{"alerts":[{"location_title":"м. Львів","location_type":"city","location_oblast":"Львівська область",
            "alert_type":"air_raid","alert_level":"red","started_at":"2026-10-22T10:00:00Z","finished_at":null}]}"#;
        let c = parse_alerts(serde_json::from_str::<ARoot>(city).unwrap().alerts);
        assert_eq!((c.hromadas[0].key.as_str(), c.hromadas[0].oblast.as_str()), ("львівський", "Львівська область"));
        assert_eq!(oblast_key("м. Севастополь"), "севастополь");
    }
}
