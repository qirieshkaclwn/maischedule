use anyhow::{Context, Result};
use chrono::NaiveDate;
use md5::{Digest, Md5};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::models::{DaySchedule, GroupInfo, GroupSchedule, Lesson};

#[allow(dead_code)]
pub const GROUPS_URL: &str = "https://public.mai.ru/schedule/data/groups.json";
pub const SCHEDULE_URL_TEMPLATE: &str = "https://public.mai.ru/schedule/data/{md5}.json";
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

pub fn get_group_hash(group_name: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(group_name.trim().as_bytes());
    format!("{:x}", hasher.finalize())
}

#[allow(dead_code)]
pub async fn fetch_groups(client: &reqwest::Client) -> Result<Vec<GroupInfo>> {
    let resp = client
        .get(GROUPS_URL)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .context("Ошибка выполнения запроса к списку групп")?;

    let groups: Vec<GroupInfo> = resp
        .json()
        .await
        .context("Ошибка десериализации списка групп")?;
    Ok(groups)
}

pub async fn fetch_schedule(client: &reqwest::Client, group_name: &str) -> Result<GroupSchedule> {
    let hash = get_group_hash(group_name);
    let url = SCHEDULE_URL_TEMPLATE.replace("{md5}", &hash);

    let resp = client
        .get(&url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .with_context(|| format!("Ошибка запроса расписания по URL: {}", url))?;

    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("API МАИ вернуло HTTP статус {} для группы {}", status, group_name);
    }

    let json_val: Value = resp
        .json()
        .await
        .context("Ошибка парсинга JSON расписания")?;

    parse_schedule_json(json_val, group_name)
}

pub fn parse_schedule_json(raw: Value, fallback_group: &str) -> Result<GroupSchedule> {
    let obj = raw
        .as_object()
        .context("Корневой элемент расписания должен быть JSON объектом")?;

    let group = obj
        .get("group")
        .and_then(|v| v.as_str())
        .unwrap_or(fallback_group)
        .trim()
        .to_string();

    let mut days: BTreeMap<String, DaySchedule> = BTreeMap::new();

    for (key, val) in obj {
        if key == "group" {
            continue;
        }

        // Парсим дату формата "DD.MM.YYYY" (например, "03.09.2026")
        if let Ok(date) = NaiveDate::parse_from_str(key, "%d.%m.%Y") {
            if let Some(day_sched) = parse_day_schedule(date, val) {
                let iso_key = date.format("%Y-%m-%d").to_string();
                days.insert(iso_key, day_sched);
            }
        }
    }

    Ok(GroupSchedule { group, days })
}

fn parse_day_schedule(date: NaiveDate, val: &Value) -> Option<DaySchedule> {
    let day_obj = val.as_object()?;
    let day_of_week = day_obj
        .get("day")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut lessons = day_obj
        .get("pairs")
        .and_then(parse_lessons_from_pairs)
        .unwrap_or_default();

    lessons.sort_by_key(|a| a.time_start_clean());

    Some(DaySchedule {
        date,
        day_of_week,
        lessons,
    })
}

fn parse_lessons_from_pairs(pairs_val: &Value) -> Option<Vec<Lesson>> {
    let pairs_obj = pairs_val.as_object()?;
    let mut lessons = Vec::new();

    for (time_key, subjects_val) in pairs_obj {
        if let Some(subjects_obj) = subjects_val.as_object() {
            for (subject_name, lesson_val) in subjects_obj {
                if let Some(l_obj) = lesson_val.as_object() {
                    let time_start = l_obj
                        .get("time_start")
                        .and_then(|v| v.as_str())
                        .unwrap_or(time_key)
                        .to_string();

                    let time_end = l_obj
                        .get("time_end")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let lectors = extract_dict_values(l_obj.get("lector"));
                    let lesson_types = extract_dict_keys(l_obj.get("type"));
                    let rooms = extract_dict_values(l_obj.get("room"));
                    let lms = extract_non_empty_str(l_obj.get("lms"));
                    let teams = extract_non_empty_str(l_obj.get("teams"));
                    let other = extract_non_empty_str(l_obj.get("other"));

                    lessons.push(Lesson {
                        subject: subject_name.trim().to_string(),
                        time_start,
                        time_end,
                        lectors,
                        lesson_types,
                        rooms,
                        lms,
                        teams,
                        other,
                    });
                }
            }
        }
    }

    Some(lessons)
}

fn extract_dict_values(val: Option<&Value>) -> Vec<String> {
    val.and_then(|v| v.as_object())
        .map(|dict| {
            dict.values()
                .filter_map(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn extract_dict_keys(val: Option<&Value>) -> Vec<String> {
    val.and_then(|v| v.as_object())
        .map(|dict| {
            dict.keys()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn extract_non_empty_str(val: Option<&Value>) -> Option<String> {
    val.and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_get_group_hash() {
        assert_eq!(
            get_group_hash("М14О-101БВ-26"),
            "debcbad33560cdb3b93c753df8221a3d"
        );
        // Trimming check
        assert_eq!(
            get_group_hash("  М14О-101БВ-26  "),
            "debcbad33560cdb3b93c753df8221a3d"
        );
    }

    #[test]
    fn test_parse_schedule_json() {
        let raw = json!({
            "group": "М14О-101БВ-26",
            "03.09.2026": {
                "day": "Чт",
                "pairs": {
                    "9:00:00": {
                        "История России": {
                            "time_start": "9:00:00",
                            "time_end": "10:30:00",
                            "lector": { "guid1": "Студников Павел Евгеньевич" },
                            "type": { "ЛК": 1 },
                            "room": { "room1": "Орш. А-301" },
                            "lms": "https://lms.mai.ru",
                            "teams": "",
                            "other": ""
                        }
                    },
                    "10:45:00": {
                        "Линейная алгебра": {
                            "time_start": "10:45:00",
                            "time_end": "12:15:00",
                            "lector": {},
                            "type": { "ПЗ": 1 },
                            "room": { "room2": "Орш. А-304" },
                            "lms": "",
                            "teams": "",
                            "other": ""
                        }
                    }
                }
            }
        });

        let sched = parse_schedule_json(raw, "FALLBACK").unwrap();
        assert_eq!(sched.group, "М14О-101БВ-26");
        assert_eq!(sched.days.len(), 1);

        let day = sched.days.get("2026-09-03").expect("Day 2026-09-03 should exist");
        assert_eq!(day.day_of_week, "Чт");
        assert_eq!(day.lessons.len(), 2);

        let l1 = &day.lessons[0];
        assert_eq!(l1.subject, "История России");
        assert_eq!(l1.time_start_clean(), "09:00");
        assert_eq!(l1.time_end_clean(), "10:30");
        assert_eq!(l1.type_str(), "ЛК");
        assert_eq!(l1.lector_str(), "Студников Павел Евгеньевич");
        assert_eq!(l1.room_str(), "Орш. А-301");
        assert_eq!(l1.lms.as_deref(), Some("https://lms.mai.ru"));

        let l2 = &day.lessons[1];
        assert_eq!(l2.subject, "Линейная алгебра");
        assert_eq!(l2.time_start_clean(), "10:45");
        assert_eq!(l2.type_str(), "ПЗ");
        assert_eq!(l2.lector_str(), "Преподаватель не указан");
        assert_eq!(l2.room_str(), "Орш. А-304");
    }
}
