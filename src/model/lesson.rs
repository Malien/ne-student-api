use chrono::NaiveDate;
use indoc::indoc;
use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgPool, PgQueryAs, Postgres};
use std::vec::Vec;

use super::account::AccountID;
use super::permission::{EntityPermission, LessonPermission, PermissionType};
use super::repeat::{DailyRepeat, MonthlyRepeat, SingleOccurrence, WeeklyRepeat};
use super::teacher::TeacherID;
use crate::types::Transaction;
use crate::uuid_wrapper;

uuid_wrapper!(LessonID);

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Lesson {
    pub id: LessonID,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub singles: Vec<SingleOccurrence>,
    pub weekly: Vec<WeeklyRepeat>,
    pub daily: Vec<DailyRepeat>,
    pub monthly: Vec<MonthlyRepeat>,
    pub teachers: Vec<TeacherID>,
}

#[derive(sqlx::FromRow)]
struct LessonBase {
    title: String,
    description: Option<String>,
}

impl Lesson {
    async fn teachers_of_lesson_by_id_in_transaction(
        transaction: &mut Transaction,
        lesson_id: &LessonID,
    ) -> sqlx::Result<Vec<TeacherID>> {
        Ok(sqlx::query_as::<_, (TeacherID,)>(
            "SELECT teacher_id FROM TeacherLesson WHERE lesson_id = $1",
        )
        .bind(lesson_id)
        .fetch_all(transaction)
        .await?
        .into_iter()
        .map(|(id,)| id)
        .collect())
    }

    pub async fn by_id(db: &PgPool, lesson_id: LessonID) -> sqlx::Result<Option<Lesson>> {
        let mut transaction = db.begin().await?;

        let base = sqlx::query_as("SELECT title, description FROM Lesson WHERE id = $1")
            .bind(&lesson_id)
            .fetch_optional(&mut transaction)
            .await?;

        let teachers =
            Lesson::teachers_of_lesson_by_id_in_transaction(&mut transaction, &lesson_id).await?;

        Ok(match base {
            None => None,
            Some(LessonBase { description, title }) => {
                let singles =
                    SingleOccurrence::of_lesson_in_transaction(&mut transaction, &lesson_id)
                        .await?;
                let daily =
                    DailyRepeat::of_lesson_in_transaction(&mut transaction, &lesson_id).await?;
                let weekly =
                    WeeklyRepeat::of_lesson_in_transaction(&mut transaction, &lesson_id).await?;
                let monthly =
                    MonthlyRepeat::of_lesson_in_transaction(&mut transaction, &lesson_id).await?;

                let res = Lesson {
                    id: lesson_id,
                    title,
                    description,
                    singles,
                    daily,
                    weekly,
                    monthly,
                    teachers,
                };

                transaction.commit().await?;
                Some(res)
            }
        })
    }

    pub async fn create(
        db: &PgPool,
        title: String,
        description: Option<String>,
        singles: Vec<SingleOccurrence>,
        daily: Vec<DailyRepeat>,
        weekly: Vec<WeeklyRepeat>,
        monthly: Vec<MonthlyRepeat>,
        owner: &AccountID,
    ) -> sqlx::Result<Lesson> {
        let mut transaction = db.begin().await?;

        let (id,): (LessonID,) =
            sqlx::query_as("INSERT INTO Lesson (title, description) VALUES ($1,$2) RETURNING id")
                .bind(&title)
                .bind(&description)
                .fetch_one(&mut transaction)
                .await?;

        SingleOccurrence::insert_in_transaction(&mut transaction, &singles, &id).await?;
        DailyRepeat::insert_in_transaction(&mut transaction, &daily, &id).await?;
        WeeklyRepeat::insert_in_transaction(&mut transaction, &weekly, &id).await?;
        MonthlyRepeat::insert_in_transaction(&mut transaction, &monthly, &id).await?;

        LessonPermission::save_in_transaction(
            &mut transaction,
            PermissionType::ReadWrite,
            &id,
            &owner,
        )
        .await?;

        transaction.commit().await?;

        Ok(Lesson {
            id,
            description,
            title,
            singles,
            daily,
            weekly,
            monthly,
            teachers: Vec::new(),
        })
    }

    pub async fn update(
        db: &PgPool,
        lesson_id: &LessonID,
        title: &Option<String>,
        singles: &Option<Vec<SingleOccurrence>>,
        daily: &Option<Vec<DailyRepeat>>,
        weekly: &Option<Vec<WeeklyRepeat>>,
        monthly: &Option<Vec<MonthlyRepeat>>,
        description: &Option<Option<String>>,
    ) -> sqlx::Result<()> {
        let mut transaction = db.begin().await?;

        if title.is_some() || description.is_some() {
            let setters = match (&title, &description) {
                (Some(_), Some(_)) => "title = $2, description = $3",
                (Some(_), None) => "title = $2",
                (None, Some(_)) => "description = $2",
                (None, None) => "",
            };

            let query_str = format!("UPDATE Lesson SET {} WHERE id = $1", setters);
            let mut query = sqlx::query::<Postgres>(&query_str[..]).bind(lesson_id);
            if let Some(title) = title {
                query = query.bind(title);
            }
            if let Some(description) = description {
                query = query.bind(description);
            }
            query.execute(&mut transaction).await?;
        }

        if let Some(singles) = singles {
            SingleOccurrence::update_in_transaction(&mut transaction, singles, lesson_id).await?;
        }

        if let Some(repeats) = daily {
            DailyRepeat::update_in_transaction(&mut transaction, repeats, lesson_id).await?;
        }

        if let Some(repeats) = weekly {
            WeeklyRepeat::update_in_transaction(&mut transaction, repeats, lesson_id).await?;
        }

        if let Some(repeats) = monthly {
            MonthlyRepeat::update_in_transaction(&mut transaction, repeats, lesson_id).await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    pub async fn delete(db: &PgPool, lesson_id: &LessonID) -> sqlx::Result<()> {
        sqlx::query("DELETE FROM Lesson WHERE id = $1")
            .bind(lesson_id)
            .execute(db)
            .await
            .map(|_| ())
    }

    pub async fn for_date(
        db: &PgPool,
        date: &NaiveDate,
        account_id: &AccountID,
    ) -> sqlx::Result<Vec<Lesson>> {
        let mut transaction = db.begin().await?;

        // TODO: Optimize query to first query user-accessible lessons
        let ids = sqlx::query_as::<_, (LessonID,)>(indoc! {"
            SELECT x.lesson_id FROM (
                SELECT DISTINCT lesson_id FROM SingleOccurrence 
                WHERE occurs_at BETWEEN $1 AND $1 + 1
                UNION
                SELECT DISTINCT lesson_id FROM LessonDailyRepeat
                WHERE repeats_on_date_daily(start_date, end_date, $1)
                UNION
                SELECT DISTINCT lesson_id FROM LessonWeeklyRepeat 
                WHERE repeats_on_date_weekly(start_date, end_date, week_day, every, $1)
                UNION
                SELECT DISTINCT lesson_id FROM LessonMonthlyRepeat 
                WHERE repeats_on_date_monthly(start_date, end_date, scheduled_time::DATE, every, $1)
            ) x
            WHERE is_read_permission(lesson_permission_for(x.lesson_id, $2))
        "})
        .bind(date)
        .bind(account_id)
        .fetch_all(&mut transaction)
        .await?;

        let mut res = Vec::<Lesson>::with_capacity(ids.len());
        // TODO: Optimize N+1 queries
        for (lesson_id,) in ids {
            let base = sqlx::query_as("SELECT title, description FROM Lesson WHERE id = $1")
                .bind(&lesson_id)
                .fetch_optional(&mut transaction)
                .await?;

            let lesson = match base {
                None => None,
                Some(LessonBase { description, title }) => {
                    let singles =
                        SingleOccurrence::of_lesson_in_transaction(&mut transaction, &lesson_id)
                            .await?;
                    let daily =
                        DailyRepeat::of_lesson_in_transaction(&mut transaction, &lesson_id).await?;
                    let weekly =
                        WeeklyRepeat::of_lesson_in_transaction(&mut transaction, &lesson_id)
                            .await?;
                    let monthly =
                        MonthlyRepeat::of_lesson_in_transaction(&mut transaction, &lesson_id)
                            .await?;

                    let teachers = Lesson::teachers_of_lesson_by_id_in_transaction(
                        &mut transaction,
                        &lesson_id,
                    )
                    .await?;

                    let res = Lesson {
                        id: lesson_id,
                        title,
                        description,
                        singles,
                        daily,
                        weekly,
                        monthly,
                        teachers,
                    };

                    Some(res)
                }
            };

            if let Some(lesson) = lesson {
                res.push(lesson);
            }
        }

        transaction.commit().await?;

        Ok(res)
    }
}
