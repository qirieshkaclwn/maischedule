use chrono::{NaiveTime, Utc};
use md5::{Digest as Md5Digest, Md5};
use sha1::{Digest as Sha1Digest, Sha1};

use crate::models::{GroupSchedule, Lesson};

fn escape_ical_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            _ => out.push(c),
        }
    }
    out
}

fn parse_naive_time(time_str: &str, default: NaiveTime) -> NaiveTime {
    let parts: Vec<&str> = time_str.split(':').collect();
    if parts.len() >= 2 {
        if let (Ok(h), Ok(m)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
            let s = if parts.len() > 2 {
                parts[2].parse::<u32>().unwrap_or(0)
            } else {
                0
            };
            if let Some(t) = NaiveTime::from_hms_opt(h, m, s) {
                return t;
            }
        }
    }
    default
}

pub fn generate_event_uid(group: &str, date_str: &str, lesson: &Lesson) -> String {
    let mut md5 = Md5::new();
    md5.update(group.trim().as_bytes());
    let grp_hash = &format!("{:x}", md5.finalize())[..8];

    let mut sha = Sha1::new();
    sha.update(lesson.subject.trim().as_bytes());
    let subj_hash = &format!("{:x}", sha.finalize())[..8];

    let time_clean = lesson.time_start_clean().replace(':', "");
    format!("mai-{}-{}-{}-{}@maischedule", grp_hash, date_str, time_clean, subj_hash)
}

pub fn generate_ical(schedule: &GroupSchedule, alert_minutes: u32, tz_name: &str) -> String {
    let mut buf = String::with_capacity(schedule.days.len() * 1024);
    let now_stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();

    buf.push_str("BEGIN:VCALENDAR\r\n");
    buf.push_str("VERSION:2.0\r\n");
    buf.push_str("PRODID:-//MAI Schedule Sync//maischedule//RU\r\n");
    buf.push_str("CALSCALE:GREGORIAN\r\n");
    buf.push_str("METHOD:PUBLISH\r\n");
    buf.push_str(&format!("X-WR-CALNAME:Расписание {}\r\n", escape_ical_text(&schedule.group)));
    buf.push_str(&format!("X-WR-CALDESC:Автоматическое расписание группы {} МАИ\r\n", escape_ical_text(&schedule.group)));
    buf.push_str(&format!("X-WR-TIMEZONE:{}\r\n", tz_name));
    buf.push_str("X-PUBLISHED-TTL:PT15M\r\n");
    buf.push_str("REFRESH-INTERVAL;VALUE=DURATION:PT15M\r\n");

    // VTIMEZONE для Europe/Moscow (UTC+3)
    buf.push_str("BEGIN:VTIMEZONE\r\n");
    buf.push_str("TZID:Europe/Moscow\r\n");
    buf.push_str("X-LIC-LOCATION:Europe/Moscow\r\n");
    buf.push_str("BEGIN:STANDARD\r\n");
    buf.push_str("TZOFFSETFROM:+0300\r\n");
    buf.push_str("TZOFFSETTO:+0300\r\n");
    buf.push_str("TZNAME:MSK\r\n");
    buf.push_str("DTSTART:19700101T000000\r\n");
    buf.push_str("END:STANDARD\r\n");
    buf.push_str("END:VTIMEZONE\r\n");

    for (date_str, day_sched) in &schedule.days {
        let lesson_date = day_sched.date;

        for lesson in &day_sched.lessons {
            let start_t = parse_naive_time(&lesson.time_start, NaiveTime::from_hms_opt(9, 0, 0).unwrap());
            let end_t = parse_naive_time(&lesson.time_end, NaiveTime::from_hms_opt(10, 30, 0).unwrap());

            let dt_start = format!("{};TZID={}:{}T{}", "DTSTART", tz_name, lesson_date.format("%Y%m%d"), start_t.format("%H%M%S"));
            let dt_end = format!("{};TZID={}:{}T{}", "DTEND", tz_name, lesson_date.format("%Y%m%d"), end_t.format("%H%M%S"));

            let uid = generate_event_uid(&schedule.group, date_str, lesson);
            let summary = format!("[{}] {}", lesson.type_str(), lesson.subject);
            let location = lesson.room_str();

            let mut desc = format!(
                "Предмет: {}\nТип занятия: {}\nПреподаватель: {}\nАудитория: {}",
                lesson.subject,
                lesson.type_str(),
                lesson.lector_str(),
                lesson.room_str()
            );
            if let Some(lms) = &lesson.lms {
                desc.push_str(&format!("\nLMS: {}", lms));
            }
            if let Some(teams) = &lesson.teams {
                desc.push_str(&format!("\nTeams: {}", teams));
            }
            if let Some(other) = &lesson.other {
                desc.push_str(&format!("\nДополнительно: {}", other));
            }

            buf.push_str("BEGIN:VEVENT\r\n");
            buf.push_str(&format!("UID:{}\r\n", uid));
            buf.push_str(&format!("DTSTAMP:{}\r\n", now_stamp));
            buf.push_str(&format!("{}\r\n", dt_start));
            buf.push_str(&format!("{}\r\n", dt_end));
            buf.push_str(&format!("SUMMARY:{}\r\n", escape_ical_text(&summary)));
            buf.push_str(&format!("LOCATION:{}\r\n", escape_ical_text(&location)));
            buf.push_str(&format!("DESCRIPTION:{}\r\n", escape_ical_text(&desc)));

            if alert_minutes > 0 {
                buf.push_str("BEGIN:VALARM\r\n");
                buf.push_str("ACTION:DISPLAY\r\n");
                buf.push_str(&format!("DESCRIPTION:Пара: {}\r\n", escape_ical_text(&lesson.subject)));
                buf.push_str(&format!("TRIGGER:-PT{}M\r\n", alert_minutes));
                buf.push_str("END:VALARM\r\n");
            }

            buf.push_str("END:VEVENT\r\n");
        }
    }

    buf.push_str("END:VCALENDAR\r\n");
    buf
}
