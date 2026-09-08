use anyhow::{Context, Result};
use chrono::NaiveDate;
use md5::{Digest, Md5};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::models::{DaySchedule, GroupInfo, GroupSchedule, Lesson};

pub const GROUPS_URL: &str = "https://public.mai.ru/schedule/data/groups.json";
pub const SCHEDULE_URL_TEMPLATE: &str = "https://public.mai.ru/schedule/data/{md5}.json";
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

pub fn get_group_hash(group_name: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(group_name.trim().as_bytes());
    format!("{:x}", hasher.finalize())
}

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
        .to_string();

    let mut days: BTreeMap<String, DaySchedule> = BTreeMap::new();

    for (key, val) in obj {
        if key == "group" {
            continue;
        }

        // Парсим дату формата "DD.MM.YYYY" (например, "03.09.2026")
        let date = match NaiveDate::parse_from_str(key, "%d.%m.%Y") {
            Ok(d) => d,
            Err(_) => continue,
        };

        let day_obj = match val.as_object() {
            Some(o) => o,
            None => continue,
        };

        let day_of_week = day_obj
            .get("day")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let mut lessons: Vec<Lesson> = Vec::new();

        if let Some(pairs_obj) = day_obj.get("pairs").and_then(|v| v.as_object()) {
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

                            // Преподаватели
                            let mut lectors = Vec::new();
                            if let Some(l_dict) = l_obj.get("lector").and_then(|v| v.as_object()) {
                                for name_val in l_dict.values() {
                                    if let Some(name) = name_val.as_str() {
                                        let t = name.trim();
                                        if !t.is_empty() {
                                            lectors.push(t.to_string());
                                        }
                                    }
                                }
                            }

                            // Типы занятия
                            let mut lesson_types = Vec::new();
                            if let Some(t_dict) = l_obj.get("type").and_then(|v| v.as_object()) {
                                for t_name in t_dict.keys() {
                                    let t = t_name.trim();
                                    if !t.is_empty() {
                                        lesson_types.push(t.to_string());
                                    }
                                }
                            }

                            // Аудитории
                            let mut rooms = Vec::new();
                            if let Some(r_dict) = l_obj.get("room").and_then(|v| v.as_object()) {
                                for r_val in r_dict.values() {
                                    if let Some(r_name) = r_val.as_str() {
                                        let t = r_name.trim();
                                        if !t.is_empty() {
                                            rooms.push(t.to_string());
                                        }
                                    }
                                }
                            }

                            let lms = l_obj
                                .get("lms")
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.trim().is_empty())
                                .map(|s| s.to_string());

                            let teams = l_obj
                                .get("teams")
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.trim().is_empty())
                                .map(|s| s.to_string());

                            let other = l_obj
                                .get("other")
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.trim().is_empty())
                                .map(|s| s.to_string());

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
        }

        lessons.sort_by(|a, b| a.time_start_clean().cmp(&b.time_start_clean()));

        let iso_key = date.format("%Y-%m-%d").to_string();
        days.insert(
            iso_key,
            DaySchedule {
                date,
                day_of_week,
                lessons,
            },
        );
    }

    Ok(GroupSchedule { group, days })
}
