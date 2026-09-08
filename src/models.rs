use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
