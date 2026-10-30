//! Telegram air-raid bot. The chat always holds exactly two bot messages:
//! 1. the card: map picture checked every INTERVAL_SECS (60). Whole Ukraine normally; the tracked
//!    oblast + neighbours when there is an alert there or around, or targets within 100 km. The
//!    tracked region (one oblast or one raion, chosen with the «⚙️ Регіон» button) is highlighted;
//! 2. the info message: alert start / important event / all clear; replaced (deleted + sent) on
//!    each new event, so Telegram notifies. See events.rs.
//!
//! Config (.env): TELEGRAM_BOT_TOKEN, ALERTS_IN_UA_TOKEN, optional ALLOWED_CHAT_IDS=1,2 and INTERVAL_SECS=60.
//! `cargo run --release -- --render out.png ["Область" ["Район"]]` renders one card locally.

mod catalog;
mod data;
mod events;
mod geo;
mod glyph;
mod region;
mod render;
mod text;
mod tg;
mod tiles;

use anyhow::{Context, Result};
use data::{Alerts, Threat};
use events::Session;
use geo::View;
use region::{Geo, Region};
use render::{Renderer, Summary};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tg::{btn, inline, Bot, TgError};
use tiny_skia::Pixmap;
use tokio::sync::Mutex;

const MAX_SESSIONS: usize = 20;
/// Current chat layout version (see Session::v).
const LAYOUT: u32 = 4;
const EXTRA_H: u32 = render::HEADER + render::FOOTER;

fn cache_dir() -> PathBuf {
    let dir = PathBuf::from("cache");
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn save_sessions(map: &HashMap<i64, Session>) {
    if let Ok(s) = serde_json::to_string(map) {
        std::fs::write(cache_dir().join("sessions.json"), s).ok();
    }
}

fn load_sessions() -> HashMap<i64, Session> {
    let Ok(b) = std::fs::read(cache_dir().join("sessions.json")) else { return HashMap::new() };
    if let Ok(m) = serde_json::from_slice::<HashMap<i64, Session>>(&b) {
        return m;
    }
    // Oldest format: chat -> card message id.
    serde_json::from_slice::<HashMap<i64, i64>>(&b)
        .map(|m| m.into_iter().map(|(c, card)| (c, Session { card, ..Default::default() })).collect())
        .unwrap_or_default()
}

/// One fetch of both sources, shared by all chats in a tick.
struct Data {
    alerts: Alerts,
    threats: Vec<Threat>,
    alerts_ok: bool,
    note: String,
}

async fn fetch(http: &reqwest::Client) -> Result<Data> {
    let (alerts, threats) = tokio::join!(data::fetch_alerts(http), data::fetch_threats(http));
    if alerts.is_err() && threats.is_err() {
        anyhow::bail!("both sources failed: {:?} / {:?}", alerts.err(), threats.err());
    }
    let mut note = String::new();
    let alerts_ok = alerts.is_ok();
    let alerts = alerts.unwrap_or_else(|e| {
        note += &format!("\n⚠️ Тривоги недоступні: {e}");
        Default::default()
    });
    let threats = threats.unwrap_or_else(|e| {
        note += &format!("\n⚠️ Позиції недоступні: {e}");
        Vec::new()
    });
    Ok(Data { alerts, threats, alerts_ok, note })
}

/// Message 3 (status), built so the notification preview says it all in the first line:
///   🟠 Тривога · Львівська обл. (Золочівський, Шептицький) · з 22:04 · Дрони
///   <source wording of the newest threat>
///   <event lines: target close, missile, new threat type, all clear>
///   (Оновлено 22:11:55)
/// The dot carries the kind: 🔴 missiles, 🟠 drones, 🟡 alert in the oblast but not in the
/// tracked raion, 🟢 none.
fn status_text(s: &Summary, event: Option<&str>, note: &str) -> String {
    let mut head = if s.level == render::Level::Clear {
        format!("🟢 Тривоги немає · {}", s.oblast)
    } else {
        let mut h = format!("{} Тривога · {}", s.level.emoji(), s.oblast);
        let raions: Vec<String> = s.oblast_raions.iter().map(|r| r.replace(" район", "")).collect();
        if !raions.is_empty() {
            h += &format!(" ({})", raions.join(", "));
        } else if s.whole_oblast {
            h += " (вся область)";
        }
        if let Some(t) = s.since {
            h += &format!(" · з {}", render::hm(t));
        }
        if s.level != render::Level::Oblast && !s.threats.is_empty() {
            h += &format!(" · {}", s.threats.join(", "));
        }
        h
    };
    if !s.other.is_empty() {
        head += &format!(" · ⚠️ {}", s.other.join(", "));
    }
    if let Some(m) = &s.message {
        head += &format!("\n{m}");
    }
    if let Some(e) = event.filter(|e| !e.is_empty()) {
        head += &format!("\n{e}");
    }
    head += note;
    head += &format!("\n(Оновлено {})", s.now.with_timezone(&chrono_tz::Europe::Kyiv).format("%H:%M:%S"));
    head
}

#[derive(Clone, Debug, Default)]
enum Menu {
    #[default]
    Main,
    Oblasts,
    Raions(usize),
}

struct App {
    bot: Bot,
    http: reqwest::Client,
    geo: Geo,
    r: Renderer,
    bases: Mutex<HashMap<String, Arc<Pixmap>>>,
    /// Regional frame per region (the search is not free).
    views: Mutex<HashMap<Region, View>>,
    sessions: Mutex<HashMap<i64, Session>>,
    ui: Mutex<HashMap<i64, Menu>>,
    busy: Mutex<HashSet<i64>>,
    pace: Mutex<HashMap<i64, Pace>>,
    last: Mutex<Option<Arc<Data>>>,
}

/// Per-chat send pacing (in memory): Telegram flood-limits bots that edit too often.
#[derive(Default)]
struct Pace {
    photo_fp: u64,
    photo_at: Option<std::time::Instant>,
    status_body: String,
    status_at: Option<std::time::Instant>,
    resend_at: Option<std::time::Instant>,
    /// 429 from Telegram: skip this chat until then.
    muted_until: Option<std::time::Instant>,
}

const PHOTO_MIN: Duration = Duration::from_secs(30);
const STATUS_REFRESH: Duration = Duration::from_secs(300);
const RESEND_MIN: Duration = Duration::from_secs(120);

fn older(t: Option<std::time::Instant>, d: Duration) -> bool {
    t.map_or(true, |t| t.elapsed() >= d)
}

/// A rendered card for one region.
struct Card {
    /// Hash of what the pictures show (alerts + rounded target positions): unchanged = no edit.
    fp: u64,
    /// Picture 1: whole Ukraine.
    png_ua: Vec<u8>,
    /// Picture 2: tracked oblast + neighbours.
    png: Vec<u8>,
    summary: Summary,
    note: String,
}

impl App {
    async fn regional_view(&self, region: &Region) -> View {
        if let Some(v) = self.views.lock().await.get(region) {
            return *v;
        }
        let v = self.geo.regional_view(region, EXTRA_H);
        self.views.lock().await.insert(region.clone(), v);
        v
    }

    async fn base(&self, v: &View) -> Result<Arc<Pixmap>> {
        if let Some(b) = self.bases.lock().await.get(&v.key()) {
            return Ok(b.clone());
        }
        let mut pm = tiles::base_map(&self.http, &cache_dir(), v).await?;
        tokio::task::block_in_place(|| self.r.decorate(v, &mut pm, &self.geo));
        let b = Arc::new(pm);
        self.bases.lock().await.insert(v.key(), b.clone());
        Ok(b)
    }

    /// Both pictures for a region: whole Ukraine + the regional map.
    async fn render(&self, d: &Data, region: &Region) -> Result<Card> {
        let summary = Summary::make(&d.alerts, &d.threats, region, &self.geo);
        let mut s_ua = summary.clone();
        s_ua.regional = false;
        let mut s_reg = summary.clone();
        s_reg.regional = true;
        let v_ua = View::fit(geo::UKRAINE, 7, EXTRA_H);
        let v_reg = self.regional_view(region).await;
        let (b_ua, b_reg) = (self.base(&v_ua).await?, self.base(&v_reg).await?);
        let (png_ua, png) = tokio::task::block_in_place(|| {
            (
                self.r.render(&v_ua, &b_ua, &self.geo, region, &d.alerts, &d.threats, &s_ua),
                self.r.render(&v_reg, &b_reg, &self.geo, region, &d.alerts, &d.threats, &s_reg),
            )
        });
        let fp = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            region.hash(&mut h);
            let a = &d.alerts;
            for a in a.raions.iter().chain(&a.oblasts).chain(&a.hromadas).chain(&a.other) {
                (&a.key, &a.level, &a.kind, a.threats.iter().map(|t| &t.kind).collect::<Vec<_>>()).hash(&mut h);
            }
            for t in d.threats.iter().filter(|t| t.is_point()) {
                (&t.id, &t.kind, (t.lat * 50.0).round() as i64, (t.lon * 50.0).round() as i64).hash(&mut h);
            }
            h.finish()
        };
        Ok(Card { fp, png_ua, png, summary: s_reg, note: d.note.clone() })
    }

    async fn data(&self) -> Result<Arc<Data>> {
        let cached = self.last.lock().await.clone();
        match cached {
            Some(d) => Ok(d),
            None => {
                let d = Arc::new(fetch(&self.http).await?);
                *self.last.lock().await = Some(d.clone());
                Ok(d)
            }
        }
    }

    async fn catalog(&self) -> Vec<(String, Vec<String>)> {
        catalog::CATALOG
            .iter()
            .map(|o| (o.name.to_string(), o.raions.iter().map(|(r, _)| r.to_string()).collect()))
            .collect()
    }

    /// Caption + buttons of the card for the chat's current menu.
    async fn face(&self, chat: i64) -> (String, Value) {
        let menu = self.ui.lock().await.get(&chat).cloned().unwrap_or_default();
        let region = self.sessions.lock().await.get(&chat).map(|s| s.region.clone()).unwrap_or_default();
        match menu {
            // The picture carries no text on the main screen: the status is message 2.
            Menu::Main => (String::new(), inline(vec![vec![btn(format!("⚙️ Регіон: {}", region.label()), "set")]])),
            Menu::Oblasts => {
                let cat = self.catalog().await;
                let mut rows: Vec<Vec<(String, String)>> = cat
                    .chunks(3)
                    .enumerate()
                    .map(|(r, ch)| {
                        ch.iter().enumerate().map(|(c, (o, _))| btn(o.replace(" область", ""), format!("o:{}", r * 3 + c))).collect()
                    })
                    .collect();
                rows.push(vec![btn("⬅️ Назад", "main")]);
                (format!("Зараз: {}.\nЯку область відстежувати?", region.label()), inline(rows))
            }
            Menu::Raions(i) => {
                let cat = self.catalog().await;
                let Some((oblast, raions)) = cat.get(i) else { return (String::new(), inline(vec![])) };
                let mut rows: Vec<Vec<(String, String)>> = vec![vec![btn(format!("Вся {}", oblast.to_lowercase()), "all")]];
                rows.extend(raions.chunks(2).enumerate().map(|(r, ch)| {
                    ch.iter().enumerate().map(|(c, n)| btn(n.replace(" район", ""), format!("r:{}", r * 2 + c))).collect()
                }));
                rows.push(vec![btn("⬅️ До областей", "set")]);
                (format!("{oblast}: вся область чи один район?"), inline(rows))
            }
        }
    }

    /// Edit one chat's card with a rendered card, then apply the info-message policy.
    /// Mute a chat after a 429 instead of sleeping the whole updater.
    async fn mute(&self, chat: i64, secs: u64) {
        eprintln!("chat {chat}: Telegram 429, muted for {secs} s");
        self.pace.lock().await.entry(chat).or_default().muted_until =
            Some(std::time::Instant::now() + Duration::from_secs(secs + 1));
    }

    async fn muted(&self, chat: i64) -> bool {
        self.pace.lock().await.get(&chat).and_then(|p| p.muted_until).is_some_and(|t| t > std::time::Instant::now())
    }

    /// Bring one chat's three messages up to date, paced: pictures only when what they show
    /// changed (and >= 30 s apart), status text only when its content changed (time stamp at most
    /// every 5 min), re-send (notification) only for major events or new target news (>= 2 min).
    async fn push(&self, chat: i64, c: &Card, alerts_ok: bool) {
        if self.muted(chat).await {
            return;
        }
        let Some((ua_id, card_id)) = self.sessions.lock().await.get(&chat).map(|s| (s.card_ua, s.card)) else { return };
        if card_id == 0 {
            // Start was blocked by a 429 earlier: send the messages now.
            return self.start(chat).await;
        }
        let photos_due = {
            let p = self.pace.lock().await;
            let p = p.get(&chat);
            p.map_or(true, |p| p.photo_fp != c.fp && older(p.photo_at, PHOTO_MIN))
        };
        if photos_due {
            for (id, png, with_kb) in [(ua_id, &c.png_ua, false), (card_id, &c.png, true)] {
                if id <= 0 {
                    continue;
                }
                let (cap, kb) = if with_kb { self.face(chat).await } else { (String::new(), inline(vec![])) };
                match self.bot.edit_photo(chat, id, png.clone(), &cap, &kb).await {
                    Ok(()) | Err(TgError::NotModified) => {}
                    Err(TgError::RetryAfter(s)) => return self.mute(chat, s).await,
                    Err(TgError::Gone(why)) => {
                        eprintln!("chat {chat}: {why}, dropping session");
                        let mut m = self.sessions.lock().await;
                        m.remove(&chat);
                        save_sessions(&m);
                        return;
                    }
                    Err(e) => eprintln!("edit {chat}: {}", e.text()),
                }
            }
            let mut p = self.pace.lock().await;
            let p = p.entry(chat).or_default();
            p.photo_fp = c.fp;
            p.photo_at = Some(std::time::Instant::now());
        }

        // Message 3: status.
        let (event, old, prev_event) = {
            let mut m = self.sessions.lock().await;
            let Some(sess) = m.get_mut(&chat) else { return };
            let prev = sess.last_event.clone();
            let ev = events::tick(sess, &c.summary, alerts_ok);
            if let Some(e) = &ev {
                if e.major || !e.lines.is_empty() {
                    sess.last_event = (!e.lines.is_empty()).then(|| e.lines.join("\n"));
                }
            }
            (ev, sess.info, prev)
        };
        let last = self.sessions.lock().await.get(&chat).and_then(|s| s.last_event.clone());
        let text = status_text(&c.summary, last.as_deref(), &c.note);
        let body = text.rsplit_once("\n(Оновлено").map(|(b, _)| b.to_string()).unwrap_or_else(|| text.clone());
        let resend = {
            let p = self.pace.lock().await;
            let resend_at = p.get(&chat).and_then(|p| p.resend_at);
            match &event {
                Some(e) if e.major => true,
                Some(e) if !e.lines.is_empty() => {
                    let fresh = events::normalized(&e.lines.join("\n")) != prev_event.as_deref().map(events::normalized).unwrap_or_default();
                    fresh && older(resend_at, RESEND_MIN)
                }
                _ => false,
            }
        };
        let edit_due = {
            let p = self.pace.lock().await;
            p.get(&chat).map_or(true, |p| p.status_body != body || older(p.status_at, STATUS_REFRESH))
        };
        let mut new = old;
        if resend || old.is_none() {
            if let Some(id) = old {
                self.bot.delete(chat, id).await.ok();
            }
            match self.bot.send_text(chat, &text).await {
                Ok(id) => new = Some(id),
                Err(TgError::RetryAfter(s)) => return self.mute(chat, s).await,
                Err(e) => eprintln!("status {chat}: {}", e.text()),
            }
            println!("chat {chat}: status re-sent ({})", event.as_ref().map(|e| if e.major { "major" } else { "targets" }).unwrap_or("missing"));
            let mut p = self.pace.lock().await;
            p.entry(chat).or_default().resend_at = Some(std::time::Instant::now());
        } else if edit_due {
            match self.bot.edit_text(chat, old.unwrap_or(0), &text).await {
                Ok(()) | Err(TgError::NotModified) => {}
                Err(TgError::RetryAfter(s)) => return self.mute(chat, s).await,
                Err(TgError::Gone(_)) => new = self.bot.send_text(chat, &text).await.ok(),
                Err(e) => eprintln!("status {chat}: {}", e.text()),
            }
        }
        if resend || old.is_none() || edit_due {
            let mut p = self.pace.lock().await;
            let p = p.entry(chat).or_default();
            p.status_body = body;
            p.status_at = Some(std::time::Instant::now());
        }
        if new != old {
            let mut m = self.sessions.lock().await;
            if let Some(sess) = m.get_mut(&chat) {
                sess.info = new;
            }
            save_sessions(&m);
        }
    }

    /// Redraw only caption + buttons (menu navigation).
    async fn redraw(&self, chat: i64) {
        let (card, region) = match self.sessions.lock().await.get(&chat) {
            Some(s) => (s.card, s.region.clone()),
            None => return,
        };
        let _ = region;
        let (cap, kb) = self.face(chat).await;
        if let Err(e) = self.bot.edit_caption(chat, card, &cap, &kb).await {
            if !matches!(e, TgError::NotModified) {
                eprintln!("caption {chat}: {}", e.text());
            }
        }
    }

    /// Re-render this chat's card now (after a region change).
    async fn refresh_now(&self, chat: i64) {
        let region = match self.sessions.lock().await.get(&chat) {
            Some(s) => s.region.clone(),
            None => return,
        };
        let Ok(d) = self.data().await else { return };
        if let Ok(c) = self.render(&d, &region).await {
            self.push(chat, &c, d.alerts_ok).await;
        }
    }

    /// /start (and migration): delete this chat's bot messages, send a fresh card.
    async fn start(&self, chat: i64) {
        let old = self.sessions.lock().await.remove(&chat);
        let region = old.as_ref().map(|o| o.region.clone()).unwrap_or_default();
        let ids: Vec<i64> = old
            .iter()
            .flat_map(|o| [Some(o.card_ua), Some(o.card), o.info, o.alert_msg, o.event_msg, o.clear_msg])
            .flatten()
            .filter(|id| *id > 0)
            .collect();
        for id in ids {
            self.bot.delete(chat, id).await.ok();
        }
        self.bot.remove_reply_keyboard(chat).await;
        self.ui.lock().await.insert(chat, Menu::Main);
        let card = match self.data().await {
            Ok(d) => self.render(&d, &region).await,
            Err(e) => Err(e),
        };
        let c = match card {
            Ok(c) => c,
            Err(e) => {
                self.bot.send_text(chat, &format!("Не вдалося отримати дані: {e}")).await.ok();
                return;
            }
        };
        // 1: whole Ukraine, 2: region (with the button), 3: status.
        // On a 429 keep a placeholder session (card 0): the updater re-runs start() when unmuted.
        let placeholder = |region: Region| Session { card: 0, region, v: LAYOUT, ..Default::default() };
        let ua = match self.bot.send_photo(chat, c.png_ua, "", &inline(vec![])).await {
            Ok(id) => id,
            Err(e) => {
                eprintln!("sendPhoto {chat}: {}", e.text());
                if let TgError::RetryAfter(s) = e {
                    self.sessions.lock().await.insert(chat, placeholder(region));
                    self.mute(chat, s).await;
                }
                return;
            }
        };
        let kb = inline(vec![vec![btn(format!("⚙️ Регіон: {}", region.label()), "set")]]);
        match self.bot.send_photo(chat, c.png, "", &kb).await {
            Ok(id) => {
                let info = self.bot.send_text(chat, &status_text(&c.summary, None, &c.note)).await.ok();
                println!("chat {chat}: ua {ua}, region {id}, status {info:?}");
                let mut m = self.sessions.lock().await;
                m.insert(chat, Session { card_ua: ua, card: id, info, region, v: LAYOUT, ..Default::default() });
                save_sessions(&m);
            }
            Err(e) => {
                eprintln!("sendPhoto {chat}: {}", e.text());
                self.bot.delete(chat, ua).await.ok();
                if let TgError::RetryAfter(s) = e {
                    self.sessions.lock().await.insert(chat, placeholder(region));
                    self.mute(chat, s).await;
                }
            }
        }
    }

    async fn on_button(&self, chat: i64, data: &str) {
        let (kind, val) = data.split_once(':').unwrap_or((data, ""));
        let i: usize = val.parse().unwrap_or(usize::MAX);
        let menu = self.ui.lock().await.get(&chat).cloned().unwrap_or_default();
        let set_menu = |m: Menu| async move { self.ui.lock().await.insert(chat, m) };
        match (kind, menu) {
            ("main", _) => {
                set_menu(Menu::Main).await;
                self.redraw(chat).await;
            }
            ("set", _) => {
                set_menu(Menu::Oblasts).await;
                self.redraw(chat).await;
            }
            ("o", _) if i < self.catalog().await.len() => {
                set_menu(Menu::Raions(i)).await;
                self.redraw(chat).await;
            }
            ("all", Menu::Raions(o)) | ("r", Menu::Raions(o)) => {
                let cat = self.catalog().await;
                let Some((oblast, raions)) = cat.get(o) else { return };
                let raion = if kind == "r" { raions.get(i).cloned() } else { None };
                if kind == "r" && raion.is_none() {
                    return;
                }
                let region = Region { oblast: oblast.clone(), raion };
                {
                    let mut m = self.sessions.lock().await;
                    if let Some(s) = m.get_mut(&chat) {
                        s.region = region;
                        s.reset_story();
                    }
                    save_sessions(&m);
                }
                set_menu(Menu::Main).await;
                self.refresh_now(chat).await;
            }
            _ => self.redraw(chat).await,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let http = reqwest::Client::builder()
        .user_agent("lviv-shahed-bot/0.2 (personal, non-commercial)")
        .timeout(Duration::from_secs(20))
        .build()?;
    let cache = cache_dir();
    let geo = Geo::new(
        data::load_shapes(&http, &cache, "raions.geojson").await.context("raions")?,
        data::load_shapes(&http, &cache, "oblasts.geojson").await.context("oblasts")?,
    );
    let token = std::env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default();
    let app = Arc::new(App {
        bot: Bot::new(&token),
        http,
        geo,
        r: Renderer::new(text::Text::load()?),
        bases: Mutex::new(HashMap::new()),
        views: Mutex::new(HashMap::new()),
        sessions: Mutex::new(load_sessions()),
        ui: Mutex::new(HashMap::new()),
        busy: Mutex::new(HashSet::new()),
        pace: Mutex::new(HashMap::new()),
        last: Mutex::new(None),
    });

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--audit") {
        // Regional frame of every oblast (size, zoom) rendered into a dir, for eyeballing.
        let dir = PathBuf::from(args.get(2).cloned().unwrap_or_else(|| "audit".into()));
        std::fs::create_dir_all(&dir)?;
        let d = app.data().await?;
        for (i, (oblast, _)) in app.catalog().await.iter().enumerate() {
            let region = Region { oblast: oblast.clone(), raion: None };
            let mut s = Summary::make(&d.alerts, &d.threats, &region, &app.geo);
            s.regional = true;
            let v = app.regional_view(&region).await;
            let base = app.base(&v).await?;
            let png = tokio::task::block_in_place(|| app.r.render(&v, &base, &app.geo, &region, &d.alerts, &d.threats, &s));
            std::fs::write(dir.join(format!("{i:02}.png")), png)?;
            println!("{i:02} {oblast}: z{} {}x{}", v.z, v.w, v.h);
        }
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--render") {
        let out = args.get(2).cloned().unwrap_or_else(|| "card.png".into());
        let region = match (args.get(3), args.get(4)) {
            (Some(o), r) => Region { oblast: o.clone(), raion: r.cloned() },
            _ => Region::default(),
        };
        let d = app.data().await?;
        let c = app.render(&d, &region).await?;
        std::fs::write(&out, &c.png)?;
        std::fs::write(out.replace(".png", "-ua.png"), &c.png_ua)?;
        println!("{}\n-> {out} (+ -ua.png)", status_text(&c.summary, None, &c.note));
        return Ok(());
    }

    anyhow::ensure!(!token.is_empty(), "TELEGRAM_BOT_TOKEN is not set (.env)");
    let allowed: Vec<i64> = std::env::var("ALLOWED_CHAT_IDS").unwrap_or_default().split(',').filter_map(|s| s.trim().parse().ok()).collect();
    let interval = std::env::var("INTERVAL_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(60u64).max(5);
    println!("bot up, {} saved session(s), every {interval} s", app.sessions.lock().await.len());

    // Clean every chat (all users) before starting: delete all bot + user messages Telegram still
    // allows (< 48 h), then send each subscribed chat a fresh card.
    let known: Vec<(i64, i64)> = app
        .sessions
        .lock()
        .await
        .iter()
        .map(|(c, x)| (*c, [Some(x.card_ua), Some(x.card), x.info, x.alert_msg, x.event_msg, x.clear_msg].into_iter().flatten().max().unwrap_or(0) + 30))
        .collect();
    for (chat, id) in &known {
        app.bot.note(*chat, *id);
    }
    let (n, tried) = app.bot.clean_all().await;
    println!("cleaned {n} chat(s), {tried} message ids");
    for (chat, _) in known {
        // Old ids are gone already; start() then only sends the fresh card.
        if let Some(s) = app.sessions.lock().await.get_mut(&chat) {
            s.card = 0;
            s.card_ua = 0;
            s.info = None;
            s.alert_msg = None;
            s.event_msg = None;
            s.clear_msg = None;
        }
        app.start(chat).await;
    }

    // Updater: fetch once, render once per region, push into every chat of that region.
    {
        let app = app.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(interval));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let targets: Vec<(i64, Region)> = app.sessions.lock().await.iter().map(|(c, s)| (*c, s.region.clone())).collect();
                if targets.is_empty() {
                    continue;
                }
                let d = match fetch(&app.http).await {
                    Ok(d) => Arc::new(d),
                    Err(e) => {
                        eprintln!("fetch: {e:#}");
                        continue;
                    }
                };
                *app.last.lock().await = Some(d.clone());
                let mut by_region: HashMap<Region, Vec<i64>> = HashMap::new();
                for (c, r) in targets {
                    by_region.entry(r).or_default().push(c);
                }
                for (region, chats) in by_region {
                    let card = match app.render(&d, &region).await {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("render {}: {e:#}", region.label());
                            continue;
                        }
                    };
                    for chat in chats {
                        // A chat mid-click is updated by its own action.
                        if app.busy.lock().await.insert(chat) {
                            app.push(chat, &card, d.alerts_ok).await;
                            app.busy.lock().await.remove(&chat);
                        }
                    }
                }
            }
        });
    }

    // Commands and buttons via long polling.
    let mut offset = 0i64;
    loop {
        let updates = match app.bot.updates(offset).await {
            Ok(u) => u,
            Err(TgError::RetryAfter(s)) => {
                tokio::time::sleep(Duration::from_secs(s)).await;
                continue;
            }
            Err(e) => {
                eprintln!("getUpdates: {}", e.text());
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
        };
        for u in updates {
            offset = u.update_id + 1;
            if let Some(cb) = u.callback_query {
                let (Some(m), Some(data)) = (cb.message, cb.data) else { continue };
                let chat = m.chat.id;
                if !allowed.is_empty() && !allowed.contains(&chat) {
                    continue;
                }
                println!("chat {chat}: [{data}]");
                let app = app.clone();
                tokio::spawn(async move {
                    if app.busy.lock().await.insert(chat) {
                        app.bot.answer(&cb.id, None).await;
                        app.on_button(chat, &data).await;
                        app.busy.lock().await.remove(&chat);
                    } else {
                        app.bot.answer(&cb.id, Some("Зачекай секунду…")).await;
                    }
                });
                continue;
            }
            let Some(m) = u.message else { continue };
            let chat = m.chat.id;
            if !allowed.is_empty() && !allowed.contains(&chat) {
                println!("ignored chat {chat} (not in ALLOWED_CHAT_IDS)");
                continue;
            }
            let text = m.text.unwrap_or_default();
            println!("chat {chat}: {text}");
            let app = app.clone();
            tokio::spawn(async move {
                app.bot.delete(chat, m.message_id).await.ok(); // keep the chat at two messages
                let cmd = text.split(['@', ' ']).next().unwrap_or("").to_string();
                let known = app.sessions.lock().await.contains_key(&chat);
                match cmd.as_str() {
                    "/stop" => {
                        let old = app.sessions.lock().await.remove(&chat);
                        save_sessions(&*app.sessions.lock().await);
                        let ids: Vec<i64> = old.iter().flat_map(|o| [Some(o.card), o.info]).flatten().collect();
                        for id in ids {
                            app.bot.delete(chat, id).await.ok();
                        }
                        app.bot.send_text(chat, "Зупинив оновлення. /start — почати знову.").await.ok();
                    }
                    "/start" | "/map" => {
                        let full = !known && app.sessions.lock().await.len() >= MAX_SESSIONS;
                        if full {
                            app.bot.send_text(chat, "Забагато активних чатів, спробуй пізніше.").await.ok();
                        } else if app.busy.lock().await.insert(chat) {
                            app.start(chat).await;
                            app.busy.lock().await.remove(&chat);
                        }
                    }
                    _ if !known => {
                        if app.busy.lock().await.insert(chat) {
                            app.start(chat).await;
                            app.busy.lock().await.remove(&chat);
                        }
                    }
                    _ => {}
                }
            });
        }
    }
}
