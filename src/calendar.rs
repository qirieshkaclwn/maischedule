use chrono::{NaiveTime, Utc};
use md5::{Digest as _, Md5};
use sha1::Sha1;

use crate::models::{GroupSchedule, Lesson};

pub fn escape_ical_text(text: &str) -> String {
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

pub fn parse_naive_time(time_str: &str, default: NaiveTime) -> NaiveTime {
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

    let fallback_start = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
    let fallback_end = NaiveTime::from_hms_opt(10, 30, 0).unwrap();

    for (date_str, day_sched) in &schedule.days {
        let lesson_date = day_sched.date;

        for lesson in &day_sched.lessons {
            let start_t = parse_naive_time(&lesson.time_start, fallback_start);
            let end_t = parse_naive_time(&lesson.time_end, fallback_end);

            let dt_start = format!(
                "DTSTART;TZID={}:{}T{}",
                tz_name,
                lesson_date.format("%Y%m%d"),
                start_t.format("%H%M%S")
            );
            let dt_end = format!(
                "DTEND;TZID={}:{}T{}",
                tz_name,
                lesson_date.format("%Y%m%d"),
                end_t.format("%H%M%S")
            );

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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use std::collections::BTreeMap;
    use crate::models::DaySchedule;

    #[test]
    fn test_escape_ical_text() {
        assert_eq!(escape_ical_text("Hello, World;"), "Hello\\, World\\;");
        assert_eq!(escape_ical_text("Line1\nLine2"), "Line1\\nLine2");
        assert_eq!(escape_ical_text("Back\\slash"), "Back\\\\slash");
    }

    #[test]
    fn test_parse_naive_time() {
        let fallback = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
        assert_eq!(
            parse_naive_time("09:30:00", fallback),
            NaiveTime::from_hms_opt(9, 30, 0).unwrap()
        );
        assert_eq!(
            parse_naive_time("14:15", fallback),
            NaiveTime::from_hms_opt(14, 15, 0).unwrap()
        );
        assert_eq!(parse_naive_time("invalid", fallback), fallback);
    }

    #[test]
    fn test_generate_event_uid() {
        let lesson = Lesson {
            subject: "Физика".to_string(),
            time_start: "09:00:00".to_string(),
            time_end: "10:30:00".to_string(),
            rooms: vec![],
            lectors: vec![],
            lesson_types: vec![],
            lms: None,
            teams: None,
            other: None,
        };
        let uid1 = generate_event_uid("М14О-101БВ-26", "2026-09-03", &lesson);
        let uid2 = generate_event_uid("М14О-101БВ-26", "2026-09-03", &lesson);
        assert_eq!(uid1, uid2);
        assert!(uid1.starts_with("mai-"));
        assert!(uid1.ends_with("@maischedule"));
    }

    #[test]
    fn test_generate_ical() {
        let date = NaiveDate::from_ymd_opt(2026, 9, 3).unwrap();
        let lesson = Lesson {
            subject: "Базы данных".to_string(),
            time_start: "10:45:00".to_string(),
            time_end: "12:15:00".to_string(),
            rooms: vec!["ГУК Б-301".to_string()],
            lectors: vec!["Кузнецов А. С.".to_string()],
            lesson_types: vec!["ЛК".to_string()],
            lms: Some("https://lms.mai.ru/course/123".to_string()),
            teams: None,
            other: None,
        };

        let mut days = BTreeMap::new();
        days.insert(
            "2026-09-03".to_string(),
            DaySchedule {
                date,
                day_of_week: "Чт".to_string(),
                lessons: vec![lesson],
            },
        );

        let schedule = GroupSchedule {
            group: "М14О-101БВ-26".to_string(),
            days,
        };

        let ical = generate_ical(&schedule, 15, "Europe/Moscow");
        assert!(ical.contains("BEGIN:VCALENDAR"));
        assert!(ical.contains("END:VCALENDAR"));
        assert!(ical.contains("X-WR-CALNAME:Расписание М14О-101БВ-26"));
        assert!(ical.contains("SUMMARY:[ЛК] Базы данных"));
        assert!(ical.contains("LOCATION:ГУК Б-301"));
        assert!(ical.contains("Кузнецов А. С."));
        assert!(ical.contains("https://lms.mai.ru/course/123"));
        assert!(ical.contains("BEGIN:VALARM"));
        assert!(ical.contains("TRIGGER:-PT15M"));
    }
}
