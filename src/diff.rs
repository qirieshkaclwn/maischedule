use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::models::{GroupSchedule, Lesson};
use crate::utils::escape_html;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeType {
    Cancelled,
    Added,
    Moved,
    RoomChanged,
    LectorChanged,
    TypeChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleChange {
    pub change_type: ChangeType,
    pub subject: String,
    pub date: NaiveDate,
    pub day_name: String,
    pub time_start: String,
    pub time_end: String,
    pub old_date: Option<NaiveDate>,
    pub old_time_start: Option<String>,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub details: String,
}

#[derive(Hash, PartialEq, Eq)]
struct SlotKey {
    date_iso: String,
    time_start: String,
    subject_lower: String,
}

fn make_key(date: &NaiveDate, lesson: &Lesson) -> SlotKey {
    SlotKey {
        date_iso: date.format("%Y-%m-%d").to_string(),
        time_start: lesson.time_start_clean(),
        subject_lower: lesson.subject.trim().to_lowercase(),
    }
}

pub fn detect_diff(old_sched: &GroupSchedule, new_sched: &GroupSchedule) -> Vec<ScheduleChange> {
    let mut changes = Vec::new();

    let mut old_map = HashMap::new();
    for day in old_sched.days.values() {
        for lesson in &day.lessons {
            let key = make_key(&day.date, lesson);
            old_map.insert(key, (day.date, day.day_of_week.clone(), lesson.clone()));
        }
    }

    let mut new_map = HashMap::new();
    for day in new_sched.days.values() {
        for lesson in &day.lessons {
            let key = make_key(&day.date, lesson);
            new_map.insert(key, (day.date, day.day_of_week.clone(), lesson.clone()));
        }
    }

    // 1. Проверяем совпавшие пары на предмет изменения аудитории, лектора или типа
    for (key, (new_date, new_day_name, new_lesson)) in &new_map {
        if let Some((_old_date, _old_day_name, old_lesson)) = old_map.get(key) {
            if old_lesson.room_str() != new_lesson.room_str() {
                changes.push(ScheduleChange {
                    change_type: ChangeType::RoomChanged,
                    subject: new_lesson.subject.clone(),
                    date: *new_date,
                    day_name: new_day_name.clone(),
                    time_start: new_lesson.time_start_clean(),
                    time_end: new_lesson.time_end_clean(),
                    old_date: None,
                    old_time_start: None,
                    old_value: Some(old_lesson.room_str()),
                    new_value: Some(new_lesson.room_str()),
                    details: format!(
                        "Аудитория изменена с «{}» на «{}»",
                        old_lesson.room_str(),
                        new_lesson.room_str()
                    ),
                });
            }

            if old_lesson.lector_str() != new_lesson.lector_str() {
                changes.push(ScheduleChange {
                    change_type: ChangeType::LectorChanged,
                    subject: new_lesson.subject.clone(),
                    date: *new_date,
                    day_name: new_day_name.clone(),
                    time_start: new_lesson.time_start_clean(),
                    time_end: new_lesson.time_end_clean(),
                    old_date: None,
                    old_time_start: None,
                    old_value: Some(old_lesson.lector_str()),
                    new_value: Some(new_lesson.lector_str()),
                    details: format!(
                        "Преподаватель изменен с «{}» на «{}»",
                        old_lesson.lector_str(),
                        new_lesson.lector_str()
                    ),
                });
            }

            if old_lesson.type_str() != new_lesson.type_str() {
                changes.push(ScheduleChange {
                    change_type: ChangeType::TypeChanged,
                    subject: new_lesson.subject.clone(),
                    date: *new_date,
                    day_name: new_day_name.clone(),
                    time_start: new_lesson.time_start_clean(),
                    time_end: new_lesson.time_end_clean(),
                    old_date: None,
                    old_time_start: None,
                    old_value: Some(old_lesson.type_str()),
                    new_value: Some(new_lesson.type_str()),
                    details: format!(
                        "Тип занятия изменен с «{}» на «{}»",
                        old_lesson.type_str(),
                        new_lesson.type_str()
                    ),
                });
            }
        }
    }

    // 2. Отмененные и добавленные
    let mut removed_vec = Vec::new();
    for (k, v) in &old_map {
        if !new_map.contains_key(k) {
            removed_vec.push(v);
        }
    }

    let mut added_vec = Vec::new();
    for (k, v) in &new_map {
        if !old_map.contains_key(k) {
            added_vec.push(v);
        }
    }

    let mut matched_removed = HashSet::new();
    let mut matched_added = HashSet::new();

    // Проверка переносов
    for (r_idx, (r_date, _r_day_name, r_lesson)) in removed_vec.iter().enumerate() {
        if matched_removed.contains(&r_idx) {
            continue;
        }

        for (a_idx, (a_date, a_day_name, a_lesson)) in added_vec.iter().enumerate() {
            if matched_added.contains(&a_idx) {
                continue;
            }

            let subj_match = r_lesson.subject.trim().eq_ignore_ascii_case(a_lesson.subject.trim());
            let type_match = r_lesson.type_str() == a_lesson.type_str();

            if subj_match && type_match {
                matched_removed.insert(r_idx);
                matched_added.insert(a_idx);

                changes.push(ScheduleChange {
                    change_type: ChangeType::Moved,
                    subject: a_lesson.subject.clone(),
                    date: *a_date,
                    day_name: a_day_name.clone(),
                    time_start: a_lesson.time_start_clean(),
                    time_end: a_lesson.time_end_clean(),
                    old_date: Some(*r_date),
                    old_time_start: Some(r_lesson.time_start_clean()),
                    old_value: Some(format!("{} {}", r_date.format("%d.%m"), r_lesson.time_start_clean())),
                    new_value: Some(format!("{} {}", a_date.format("%d.%m"), a_lesson.time_start_clean())),
                    details: format!(
                        "Перенесено с {} {} на {} {}",
                        r_date.format("%d.%m"),
                        r_lesson.time_start_clean(),
                        a_date.format("%d.%m"),
                        a_lesson.time_start_clean()
                    ),
                });
                break;
            }
        }
    }

    // Чисто отмененные
    for (r_idx, (r_date, r_day_name, r_lesson)) in removed_vec.iter().enumerate() {
        if !matched_removed.contains(&r_idx) {
            changes.push(ScheduleChange {
                change_type: ChangeType::Cancelled,
                subject: r_lesson.subject.clone(),
                date: *r_date,
                day_name: r_day_name.clone(),
                time_start: r_lesson.time_start_clean(),
                time_end: r_lesson.time_end_clean(),
                old_date: None,
                old_time_start: None,
                old_value: None,
                new_value: None,
                details: format!("Занятие отменено: {} [{}]", r_lesson.subject, r_lesson.type_str()),
            });
        }
    }

    // Чисто добавленные
    for (a_idx, (a_date, a_day_name, a_lesson)) in added_vec.iter().enumerate() {
        if !matched_added.contains(&a_idx) {
            changes.push(ScheduleChange {
                change_type: ChangeType::Added,
                subject: a_lesson.subject.clone(),
                date: *a_date,
                day_name: a_day_name.clone(),
                time_start: a_lesson.time_start_clean(),
                time_end: a_lesson.time_end_clean(),
                old_date: None,
                old_time_start: None,
                old_value: None,
                new_value: Some(a_lesson.room_str()),
                details: format!(
                    "Добавлено занятие: {} [{}] в ауд. {}",
                    a_lesson.subject,
                    a_lesson.type_str(),
                    a_lesson.room_str()
                ),
            });
        }
    }

    changes.sort_by(|a, b| (a.date, &a.time_start).cmp(&(b.date, &b.time_start)));
    changes
}

/// Фильтрует изменения расписания, исключая прошедшие даты (оставляет сегодня и будущие).
pub fn filter_current_and_future_changes(
    changes: &[ScheduleChange],
    reference_date: NaiveDate,
) -> Vec<ScheduleChange> {
    changes
        .iter()
        .filter(|c| c.date >= reference_date || c.old_date.is_some_and(|d| d >= reference_date))
        .cloned()
        .collect()
}

pub fn format_diff_message(group_name: &str, changes: &[ScheduleChange]) -> String {
    let safe_group = escape_html(group_name);
    if changes.is_empty() {
        return format!("Расписание группы {} проверено — изменений нет.", safe_group);
    }

    let mut cancelled = Vec::new();
    let mut moved = Vec::new();
    let mut room_changed = Vec::new();
    let mut lector_changed = Vec::new();
    let mut type_changed = Vec::new();
    let mut added = Vec::new();

    for c in changes {
        match c.change_type {
            ChangeType::Cancelled => cancelled.push(c),
            ChangeType::Moved => moved.push(c),
            ChangeType::RoomChanged => room_changed.push(c),
            ChangeType::LectorChanged => lector_changed.push(c),
            ChangeType::TypeChanged => type_changed.push(c),
            ChangeType::Added => added.push(c),
        }
    }

    let mut out = format!("<b>Изменения в расписании группы {}!</b>\n\n", safe_group);

    if !cancelled.is_empty() {
        out.push_str("<b>Отмена занятий:</b>\n");
        for c in cancelled {
            out.push_str(&format!(
                "- <b>{} ({}) {}</b> — {}\n",
                c.date.format("%d.%m.%Y"),
                escape_html(&c.day_name),
                c.time_start,
                escape_html(&c.subject)
            ));
        }
        out.push('\n');
    }

    if !moved.is_empty() {
        out.push_str("<b>Перенос занятий:</b>\n");
        for c in moved {
            let old = escape_html(c.old_value.as_deref().unwrap_or(""));
            let new = escape_html(c.new_value.as_deref().unwrap_or(""));
            out.push_str(&format!(
                "- <b>{}</b>: с <s>{}</s> -> на <b>{}</b>\n",
                escape_html(&c.subject),
                old,
                new
            ));
        }
        out.push('\n');
    }

    if !room_changed.is_empty() {
        out.push_str("<b>Изменение аудитории:</b>\n");
        for c in room_changed {
            let old = escape_html(c.old_value.as_deref().unwrap_or(""));
            let new = escape_html(c.new_value.as_deref().unwrap_or(""));
            out.push_str(&format!(
                "- <b>{} {}</b> — {}:\n   <s>{}</s> -> <b>{}</b>\n",
                c.date.format("%d.%m"),
                c.time_start,
                escape_html(&c.subject),
                old,
                new
            ));
        }
        out.push('\n');
    }

    if !lector_changed.is_empty() {
        out.push_str("<b>Смена преподавателя:</b>\n");
        for c in lector_changed {
            let old = escape_html(c.old_value.as_deref().unwrap_or(""));
            let new = escape_html(c.new_value.as_deref().unwrap_or(""));
            out.push_str(&format!(
                "- <b>{} {}</b> — {}:\n   <s>{}</s> -> <b>{}</b>\n",
                c.date.format("%d.%m"),
                c.time_start,
                escape_html(&c.subject),
                old,
                new
            ));
        }
        out.push('\n');
    }

    if !type_changed.is_empty() {
        out.push_str("<b>Изменение типа занятия:</b>\n");
        for c in type_changed {
            let old = escape_html(c.old_value.as_deref().unwrap_or(""));
            let new = escape_html(c.new_value.as_deref().unwrap_or(""));
            out.push_str(&format!(
                "- <b>{} {}</b> — {}:\n   <s>{}</s> -> <b>{}</b>\n",
                c.date.format("%d.%m"),
                c.time_start,
                escape_html(&c.subject),
                old,
                new
            ));
        }
        out.push('\n');
    }

    if !added.is_empty() {
        out.push_str("<b>Добавлены новые занятия:</b>\n");
        for c in added {
            let room_info = c
                .new_value
                .as_ref()
                .map(|r| format!(" (ауд. {})", escape_html(r)))
                .unwrap_or_default();
            out.push_str(&format!(
                "- <b>{} ({}) {}</b> — {}{}\n",
                c.date.format("%d.%m.%Y"),
                escape_html(&c.day_name),
                c.time_start,
                escape_html(&c.subject),
                room_info
            ));
        }
        out.push('\n');
    }

    out.push_str("<i>События в календаре на iPhone синхронизируются автоматически.</i>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::DaySchedule;
    use std::collections::BTreeMap;

    fn make_lesson(
        subject: &str,
        start: &str,
        end: &str,
        room: &str,
        lectors: Vec<&str>,
        types: Vec<&str>,
    ) -> Lesson {
        Lesson {
            subject: subject.to_string(),
            time_start: start.to_string(),
            time_end: end.to_string(),
            rooms: vec![room.to_string()],
            lectors: lectors.into_iter().map(ToString::to_string).collect(),
            lesson_types: types.into_iter().map(ToString::to_string).collect(),
            lms: None,
            teams: None,
            other: None,
        }
    }

    fn make_schedule(date: NaiveDate, day_of_week: &str, lessons: Vec<Lesson>) -> GroupSchedule {
        let mut days = BTreeMap::new();
        days.insert(
            date.format("%Y-%m-%d").to_string(),
            DaySchedule {
                date,
                day_of_week: day_of_week.to_string(),
                lessons,
            },
        );
        GroupSchedule {
            group: "TEST-1".to_string(),
            days,
        }
    }

    #[test]
    fn test_detect_room_and_lector_changes() {
        let d = NaiveDate::from_ymd_opt(2026, 9, 3).unwrap();
        let old_sched = make_schedule(
            d,
            "Чт",
            vec![make_lesson(
                "Физика",
                "09:00:00",
                "10:30:00",
                "101",
                vec!["Иванов"],
                vec!["ЛК"],
            )],
        );
        let new_sched = make_schedule(
            d,
            "Чт",
            vec![make_lesson(
                "Физика",
                "09:00:00",
                "10:30:00",
                "202",
                vec!["Петров"],
                vec!["ЛК"],
            )],
        );

        let diff = detect_diff(&old_sched, &new_sched);
        let types: Vec<ChangeType> = diff.iter().map(|c| c.change_type.clone()).collect();
        assert!(types.contains(&ChangeType::RoomChanged));
        assert!(types.contains(&ChangeType::LectorChanged));

        let msg = format_diff_message("TEST-1", &diff);
        assert!(msg.contains("Изменение аудитории"));
        assert!(msg.contains("202"));
        assert!(msg.contains("Смена преподавателя"));
        assert!(msg.contains("Петров"));
    }

    #[test]
    fn test_detect_moved_lesson() {
        let d1 = NaiveDate::from_ymd_opt(2026, 9, 3).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 9, 4).unwrap();

        let old_sched = make_schedule(
            d1,
            "Чт",
            vec![make_lesson(
                "Математика",
                "09:00:00",
                "10:30:00",
                "101",
                vec!["Иванов"],
                vec!["ЛК"],
            )],
        );
        let new_sched = make_schedule(
            d2,
            "Пт",
            vec![make_lesson(
                "Математика",
                "10:45:00",
                "12:15:00",
                "101",
                vec!["Иванов"],
                vec!["ЛК"],
            )],
        );

        let diff = detect_diff(&old_sched, &new_sched);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].change_type, ChangeType::Moved);
        assert_eq!(diff[0].subject, "Математика");
        assert_eq!(diff[0].old_date, Some(d1));
        assert_eq!(diff[0].date, d2);

        let msg = format_diff_message("TEST-1", &diff);
        assert!(msg.contains("Перенос занятий"));
    }

    #[test]
    fn test_detect_cancelled_and_added() {
        let d = NaiveDate::from_ymd_opt(2026, 9, 3).unwrap();
        let old_sched = make_schedule(
            d,
            "Чт",
            vec![make_lesson(
                "История",
                "09:00:00",
                "10:30:00",
                "101",
                vec![],
                vec!["ЛК"],
            )],
        );
        let new_sched = make_schedule(
            d,
            "Чт",
            vec![make_lesson(
                "Информатика",
                "10:45:00",
                "12:15:00",
                "102",
                vec![],
                vec!["ПЗ"],
            )],
        );

        let diff = detect_diff(&old_sched, &new_sched);
        let types: Vec<ChangeType> = diff.iter().map(|c| c.change_type.clone()).collect();
        assert!(types.contains(&ChangeType::Cancelled));
        assert!(types.contains(&ChangeType::Added));

        let msg = format_diff_message("TEST-1", &diff);
        assert!(msg.contains("Отмена занятий"));
        assert!(msg.contains("История"));
        assert!(msg.contains("Добавлены новые занятия"));
        assert!(msg.contains("Информатика"));
    }

    #[test]
    fn test_detect_type_changed() {
        let d = NaiveDate::from_ymd_opt(2026, 9, 3).unwrap();
        let old_sched = make_schedule(
            d,
            "Чт",
            vec![make_lesson(
                "Физика",
                "09:00:00",
                "10:30:00",
                "101",
                vec![],
                vec!["Лекция"],
            )],
        );
        let new_sched = make_schedule(
            d,
            "Чт",
            vec![make_lesson(
                "Физика",
                "09:00:00",
                "10:30:00",
                "101",
                vec![],
                vec!["Практика"],
            )],
        );

        let diff = detect_diff(&old_sched, &new_sched);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].change_type, ChangeType::TypeChanged);

        let msg = format_diff_message("TEST-1", &diff);
        assert!(msg.contains("Изменение типа занятия"));
        assert!(msg.contains("Лекция"));
        assert!(msg.contains("Практика"));
    }

    #[test]
    fn test_format_diff_message_html_escape() {
        let d = NaiveDate::from_ymd_opt(2026, 9, 3).unwrap();
        let changes = vec![ScheduleChange {
            change_type: ChangeType::Cancelled,
            subject: "C++ & Алгоритмы <1>".to_string(),
            date: d,
            day_name: "Чт".to_string(),
            time_start: "09:00".to_string(),
            time_end: "10:30".to_string(),
            old_date: None,
            old_time_start: None,
            old_value: None,
            new_value: None,
            details: "cancelled".to_string(),
        }];

        let msg = format_diff_message("TEST <&>", &changes);
        assert!(msg.contains("TEST &lt;&amp;&gt;"));
        assert!(msg.contains("C++ &amp; Алгоритмы &lt;1&gt;"));
    }

    #[test]
    fn test_filter_current_and_future_changes() {
        let past_date = NaiveDate::from_ymd_opt(2026, 9, 7).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
        let future_date = NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();

        let changes = vec![
            ScheduleChange {
                change_type: ChangeType::Cancelled,
                subject: "Прошедшая пара".to_string(),
                date: past_date,
                day_name: "Пн".to_string(),
                time_start: "14:45".to_string(),
                time_end: "16:15".to_string(),
                old_date: None,
                old_time_start: None,
                old_value: None,
                new_value: None,
                details: "cancelled".to_string(),
            },
            ScheduleChange {
                change_type: ChangeType::Cancelled,
                subject: "Будущая пара".to_string(),
                date: future_date,
                day_name: "Пн".to_string(),
                time_start: "14:45".to_string(),
                time_end: "16:15".to_string(),
                old_date: None,
                old_time_start: None,
                old_value: None,
                new_value: None,
                details: "cancelled".to_string(),
            },
            ScheduleChange {
                change_type: ChangeType::Moved,
                subject: "Перенос из прошлого в будущее".to_string(),
                date: future_date,
                day_name: "Пн".to_string(),
                time_start: "10:45".to_string(),
                time_end: "12:15".to_string(),
                old_date: Some(past_date),
                old_time_start: Some("14:45".to_string()),
                old_value: Some("07.09".to_string()),
                new_value: Some("14.09".to_string()),
                details: "moved".to_string(),
            },
        ];

        let filtered = filter_current_and_future_changes(&changes, today);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].subject, "Будущая пара");
        assert_eq!(filtered[1].subject, "Перенос из прошлого в будущее");
    }
}
