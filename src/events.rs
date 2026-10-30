//! Per-chat message policy: the chat holds the live card + ONE info message. The info message is
//! replaced (old one deleted, new one sent, so Telegram notifies) when:
//! - an alert starts in the tracked region,
//! - something important happens (a target within 100 / 30 km, a missile within 300 km,
//!   the threat level changes, a new threat type such as ballistic missiles),
//! - the alert ends ("відбій").

use crate::region::Region;
use crate::render::{hm, Level, Summary, CLOSE_KM, NEAR_KM, WATCH_KM};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    /// Picture 1: whole Ukraine, edited every tick.
    #[serde(default)]
    pub card_ua: i64,
    /// Picture 2: tracked oblast + neighbours (with the region button), edited every tick.
    pub card: i64,
    /// The single info message after the card.
    #[serde(default)]
    pub info: Option<i64>,
    /// Latest event line(s) shown in the status message until the next event.
    #[serde(default)]
    pub last_event: Option<String>,
    #[serde(default)]
    pub region: Region,
    /// Layout version; older sessions are migrated at boot.
    #[serde(default)]
    pub v: u32,
    /// Messages of the old 3-slot layout, deleted on migration.
    #[serde(default, skip_serializing)]
    pub alert_msg: Option<i64>,
    #[serde(default, skip_serializing)]
    pub event_msg: Option<i64>,
    #[serde(default, skip_serializing)]
    pub clear_msg: Option<i64>,
    /// Region level seen on the previous tick; None until the first tick after (re)start.
    #[serde(default)]
    pub level: Option<Level>,
    #[serde(default)]
    pub alert_since: Option<DateTime<Utc>>,
    /// Tracks already reported within NEAR_KM / CLOSE_KM / as missiles.
    #[serde(default)]
    pub near_seen: BTreeSet<String>,
    #[serde(default)]
    pub close_seen: BTreeSet<String>,
    #[serde(default)]
    pub missile_seen: BTreeSet<String>,
    /// alerts.in.ua threat types ("Балістика") already reported during this alert.
    #[serde(default)]
    pub threats_seen: BTreeSet<String>,
}

impl Session {
    /// Forget the alert story (after a region change): the next tick only records the state.
    pub fn reset_story(&mut self) {
        self.level = None;
        self.alert_since = None;
        self.near_seen.clear();
        self.close_seen.clear();
        self.missile_seen.clear();
        self.threats_seen.clear();
    }
}

/// Some(lines) when the status message has to be re-sent (so Telegram notifies): an alert
/// started or ended, the level changed, or a target came close. `lines` are the event lines shown
/// under the status head (empty when the head itself says it all, e.g. a new alert).
/// `alerts_ok`: the alert source answered this tick; otherwise level transitions are skipped
/// (an empty answer must not look like an all-clear).
/// What happened this tick. `major` = alert start / all clear / level change (always re-sent);
/// target lines alone are re-sent only when they say something new (see main.rs pacing).
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub lines: Vec<String>,
    pub major: bool,
}

pub fn tick(sess: &mut Session, s: &Summary, alerts_ok: bool) -> Option<Event> {
    let now = s.now;

    // First tick after (re)start: remember the state, report nothing already in progress.
    let Some(prev) = sess.level else {
        if alerts_ok {
            sess.level = Some(s.level);
            if s.level.is_alert() {
                sess.alert_since = s.since.or(Some(now));
            }
            sess.threats_seen = s.threats.iter().cloned().collect();
        }
        for (id, d, k) in &s.tracks {
            if *d <= NEAR_KM {
                sess.near_seen.insert(id.clone());
            }
            if *d <= CLOSE_KM {
                sess.close_seen.insert(id.clone());
            }
            if k.is_missile() {
                sess.missile_seen.insert(id.clone());
            }
        }
        return None;
    };

    let level = if alerts_ok { s.level } else { prev };
    let mut notify = false;
    let mut lines: Vec<String> = vec![];
    if alerts_ok && !prev.is_alert() && level.is_alert() {
        sess.alert_since = s.since.or(Some(now));
        notify = true; // the status head already says "Повітряна тривога"
    } else if alerts_ok && prev.is_alert() && !level.is_alert() {
        let mut t = format!("Відбій о {}", hm(now));
        if let Some(m) = sess.alert_since.map(|t| (now - t).num_minutes().max(0)) {
            t += &format!(" · тривала {m} хв");
        }
        lines.push(t);
        sess.alert_since = None;
    } else if alerts_ok && prev != level && (prev.is_alert() || level.is_alert()) {
        notify = true; // e.g. drones -> missiles; the head shows the new level
    }

    // A threat type new for this alert (ballistic after drones): its own line and a notification.
    // At alert start the head already lists them.
    if alerts_ok {
        let at = hm(now);
        if prev.is_alert() && level.is_alert() {
            for t in s.threats.iter().filter(|t| !sess.threats_seen.contains(*t)) {
                lines.push(format!("🚨 {t} · {at}"));
                notify = true;
            }
        }
        sess.threats_seen = if level.is_alert() { s.threats.iter().cloned().collect() } else { BTreeSet::new() };
    }

    // Targets.
    let at = hm(now);
    let mut new_near = vec![];
    for (id, d, k) in &s.tracks {
        if *d <= CLOSE_KM && sess.close_seen.insert(id.clone()) {
            sess.near_seen.insert(id.clone());
            lines.push(format!("‼️ {} за {} км · {at}", k.short(), *d as i32));
        } else if *d <= NEAR_KM && sess.near_seen.insert(id.clone()) {
            new_near.push((*d, *k));
        }
        if k.is_missile() && *d <= WATCH_KM && sess.missile_seen.insert(id.clone()) {
            lines.push(format!("🚀 {} за {} км · {at}", k.short(), *d as i32));
        }
    }
    if let Some((d, k)) = new_near.first() {
        let more = if new_near.len() > 1 { format!(" (+{} до {} км)", new_near.len() - 1, NEAR_KM as i32) } else { String::new() };
        lines.push(format!("⚠️ {} за {} км{more} · {at}", k.short(), *d as i32));
    }
    // Forget tracks that left the watch area so a later return is reported again.
    let live: BTreeSet<&String> = s.tracks.iter().map(|(id, _, _)| id).collect();
    sess.near_seen.retain(|id| live.contains(id));
    sess.close_seen.retain(|id| live.contains(id));
    sess.missile_seen.retain(|id| live.contains(id));
    sess.level = Some(level);

    let major = notify || lines.first().is_some_and(|l| l.starts_with("Відбій"));
    if lines.is_empty() && !major {
        return None;
    }
    lines.truncate(5);
    Some(Event { lines, major })
}

/// Event lines without their "· HH:MM" stamps, to tell a really new event from a repeat.
pub fn normalized(lines: &str) -> String {
    lines.lines().map(|l| l.rsplit_once(" · ").map(|(a, _)| a).unwrap_or(l)).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyph::Kind;

    fn summary(level: Level, tracks: Vec<(&str, f64, Kind)>) -> Summary {
        Summary {
            level,
            since: None,
            oblast_raions: vec![],
            threats: vec![],
            message: None,
            other: vec![],
            near: vec![],
            tracks: tracks.into_iter().map(|(i, d, k)| (i.to_string(), d, k)).collect(),
            now: Utc::now(),
            label: "Львівський р-н".into(),
            oblast: "Львівська обл.".into(),
            whole_oblast: false,
            regional: true,
        }
    }

    #[test]
    fn alert_event_clear_cycle() {
        let mut s = Session::default();
        assert!(tick(&mut s, &summary(Level::Clear, vec![]), true).is_none());
        // Alert start: notify, no extra line (the head says it).
        assert_eq!(tick(&mut s, &summary(Level::Yellow, vec![]), true), Some(Event { lines: vec![], major: true }));
        let l = tick(&mut s, &summary(Level::Yellow, vec![("t1", 80.0, Kind::Uav)]), true).unwrap();
        assert!(l.lines[0].contains("80 км") && !l.major);
        assert!(tick(&mut s, &summary(Level::Yellow, vec![("t1", 70.0, Kind::Uav)]), true).is_none());
        assert!(tick(&mut s, &summary(Level::Yellow, vec![("t1", 25.0, Kind::Uav)]), true).unwrap().lines[0].contains("‼️"));
        // Source outage must not look like an all-clear.
        assert!(tick(&mut s, &summary(Level::Clear, vec![]), false).is_none());
        let e = tick(&mut s, &summary(Level::Clear, vec![]), true).unwrap();
        assert!(e.lines[0].starts_with("Відбій") && e.major);
        assert_eq!(normalized("⚠️ Шахед за 80 км · 14:52"), "⚠️ Шахед за 80 км");
    }

    #[test]
    fn new_threat_type_notifies() {
        let mut s = Session::default();
        let with = |level, t: &[&str]| Summary { threats: t.iter().map(|x| x.to_string()).collect(), ..summary(level, vec![]) };
        assert!(tick(&mut s, &with(Level::Clear, &[]), true).is_none());
        assert!(tick(&mut s, &with(Level::Yellow, &["Дрони"]), true).unwrap().lines.is_empty());
        assert!(tick(&mut s, &with(Level::Yellow, &["Дрони"]), true).is_none());
        let e = tick(&mut s, &with(Level::Red, &["Балістика", "Дрони"]), true).unwrap();
        assert!(e.major && e.lines == vec![format!("🚨 Балістика · {}", hm(Utc::now()))]);
    }
}
