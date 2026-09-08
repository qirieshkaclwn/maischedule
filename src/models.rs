use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupInfo {
    pub name: String,
    #[serde(default)]
    pub fac: String,
    #[serde(default)]
    pub level: String,
    #[serde(default)]
    pub course: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lesson {
    pub subject: String,
    pub time_start: String,
    pub time_end: String,
    #[serde(default)]
    pub lectors: Vec<String>,
    #[serde(default)]
    pub lesson_types: Vec<String>,
    #[serde(default)]
    pub rooms: Vec<String>,
    #[serde(default)]
    pub lms: Option<String>,
    #[serde(default)]
    pub teams: Option<String>,
    #[serde(default)]
    pub other: Option<String>,
}

impl Lesson {
    pub fn type_str(&self) -> String {
        if self.lesson_types.is_empty() {
            "Занятие".to_string()
        } else {
            self.lesson_types.join(", ")
        }
    }

    pub fn lector_str(&self) -> String {
        let cleaned: Vec<&str> = self
            .lectors
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if cleaned.is_empty() {
            "Преподаватель не указан".to_string()
        } else {
            cleaned.join(", ")
        }
    }

    pub fn room_str(&self) -> String {
        let cleaned: Vec<&str> = self
            .rooms
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if cleaned.is_empty() {
            "Аудитория не указана".to_string()
        } else {
            cleaned.join(", ")
        }
    }

    pub fn time_start_clean(&self) -> String {
        format_time_hh_mm(&self.time_start)
    }

    pub fn time_end_clean(&self) -> String {
        format_time_hh_mm(&self.time_end)
    }
}

fn format_time_hh_mm(time_str: &str) -> String {
    let parts: Vec<&str> = time_str.split(':').collect();
    if parts.len() >= 2 {
        if let (Ok(h), Ok(m)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
            return format!("{:02}:{:02}", h, m);
        }
    }
    time_str.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaySchedule {
    pub date: NaiveDate,
    pub day_of_week: String,
    pub lessons: Vec<Lesson>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupSchedule {
    pub group: String,
    pub days: BTreeMap<String, DaySchedule>, // "YYYY-MM-DD" -> DaySchedule
}

impl GroupSchedule {
    pub fn get_day(&self, target_date: &NaiveDate) -> Option<&DaySchedule> {
        let key = target_date.format("%Y-%m-%d").to_string();
        self.days.get(&key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_time_hh_mm() {
        assert_eq!(format_time_hh_mm("9:00:00"), "09:00");
        assert_eq!(format_time_hh_mm("09:00:00"), "09:00");
        assert_eq!(format_time_hh_mm("10:45:00"), "10:45");
        assert_eq!(format_time_hh_mm("9:5"), "09:05");
        assert_eq!(format_time_hh_mm("invalid"), "invalid");
    }

    #[test]
    fn test_lesson_helpers() {
        let lesson = Lesson {
            subject: "Физика".to_string(),
            time_start: "9:00:00".to_string(),
            time_end: "10:30:00".to_string(),
            lectors: vec![" Иванов И.И. ".to_string(), "".to_string()],
            lesson_types: vec!["ЛК".to_string(), "ПЗ".to_string()],
            rooms: vec![" 101 ".to_string()],
            lms: Some("https://lms.mai.ru".to_string()),
            teams: None,
            other: None,
        };

        assert_eq!(lesson.type_str(), "ЛК, ПЗ");
        assert_eq!(lesson.lector_str(), "Иванов И.И.");
        assert_eq!(lesson.room_str(), "101");
        assert_eq!(lesson.time_start_clean(), "09:00");
        assert_eq!(lesson.time_end_clean(), "10:30");

        let empty_lesson = Lesson {
            subject: "Химия".to_string(),
            time_start: "10:45:00".to_string(),
            time_end: "12:15:00".to_string(),
            lectors: vec![],
            lesson_types: vec![],
            rooms: vec![],
            lms: None,
            teams: None,
            other: None,
        };

        assert_eq!(empty_lesson.type_str(), "Занятие");
        assert_eq!(empty_lesson.lector_str(), "Преподаватель не указан");
        assert_eq!(empty_lesson.room_str(), "Аудитория не указана");
    }

    #[test]
    fn test_group_schedule_get_day() {
        let date = NaiveDate::from_ymd_opt(2026, 9, 3).unwrap();
        let mut days = BTreeMap::new();
        days.insert(
            "2026-09-03".to_string(),
            DaySchedule {
                date,
                day_of_week: "Чт".to_string(),
                lessons: vec![],
            },
        );
        let schedule = GroupSchedule {
            group: "М14О-101БВ-26".to_string(),
            days,
        };

        assert!(schedule.get_day(&date).is_some());
        assert_eq!(schedule.get_day(&date).unwrap().day_of_week, "Чт");

        let other_date = NaiveDate::from_ymd_opt(2026, 9, 4).unwrap();
        assert!(schedule.get_day(&other_date).is_none());
    }
}
