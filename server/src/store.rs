use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::pb::aliceandbob::v1 as pb;

/// SQLite persistence: matches, ordered event log, round results, memo lineage.
#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS matches (
    id            TEXT PRIMARY KEY,
    agent_a       TEXT NOT NULL,
    agent_b       TEXT NOT NULL,
    gm_agent      TEXT NOT NULL,
    status        INTEGER NOT NULL,
    game_title    TEXT NOT NULL DEFAULT '',
    spec_json     TEXT,
    total_score_a REAL,
    total_score_b REAL,
    winner        TEXT,
    created_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS match_events (
    match_id   TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    event_json TEXT NOT NULL,
    PRIMARY KEY (match_id, seq)
);
CREATE TABLE IF NOT EXISTS rounds (
    match_id    TEXT NOT NULL,
    round       INTEGER NOT NULL,
    result_json TEXT NOT NULL,
    PRIMARY KEY (match_id, round)
);
CREATE TABLE IF NOT EXISTS memos (
    id            TEXT PRIMARY KEY,
    agent_id      TEXT NOT NULL,
    version       INTEGER NOT NULL,
    content       TEXT NOT NULL,
    match_id      TEXT NOT NULL DEFAULT '',
    created_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

impl Store {
    pub async fn open(path: &str) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .context("opening sqlite database")?;
        sqlx::raw_sql(SCHEMA).execute(&pool).await?;
        Ok(Self { pool })
    }

    // --- matches ---

    pub async fn create_match(
        &self,
        id: &str,
        agent_a: &str,
        agent_b: &str,
        gm_agent: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO matches (id, agent_a, agent_b, gm_agent, status, created_at_ms)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(agent_a)
        .bind(agent_b)
        .bind(gm_agent)
        .bind(pb::MatchStatus::Pending as i32)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_match_status(&self, id: &str, status: pb::MatchStatus) -> Result<()> {
        sqlx::query("UPDATE matches SET status = ? WHERE id = ?")
            .bind(status as i32)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Marks unfinished matches as failed on startup (e.g. after a container restart
    /// killed in-flight background tasks).
    pub async fn fail_interrupted_matches(&self) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT id FROM matches WHERE status NOT IN (?, ?)",
        )
        .bind(pb::MatchStatus::Completed as i32)
        .bind(pb::MatchStatus::Failed as i32)
        .fetch_all(&self.pool)
        .await?;
        let message = "Match interrupted (server restarted while this match was running).";
        let mut ids = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.get("id");
            self.fail_match_with_error(&id, message).await?;
            ids.push(id);
        }
        Ok(ids)
    }

    pub async fn fail_match_with_error(&self, id: &str, message: &str) -> Result<()> {
        self.set_match_status(id, pb::MatchStatus::Failed).await?;
        let event = pb::MatchEvent {
            match_id: id.to_string(),
            timestamp_ms: now_ms(),
            event: Some(pb::match_event::Event::MatchError(
                pb::match_event::MatchError {
                    message: message.to_string(),
                },
            )),
        };
        self.append_event(id, &event).await?;
        Ok(())
    }

    pub fn is_active_status(status: pb::MatchStatus) -> bool {
        matches!(
            status,
            pb::MatchStatus::Pending
                | pb::MatchStatus::GeneratingGame
                | pb::MatchStatus::InProgress
                | pb::MatchStatus::Reflecting
        )
    }

    pub async fn set_match_spec(&self, id: &str, title: &str, spec_json: &str) -> Result<()> {
        sqlx::query("UPDATE matches SET game_title = ?, spec_json = ? WHERE id = ?")
            .bind(title)
            .bind(spec_json)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn finish_match(
        &self,
        id: &str,
        score_a: f64,
        score_b: f64,
        winner: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE matches SET status = ?, total_score_a = ?, total_score_b = ?, winner = ?
             WHERE id = ?",
        )
        .bind(pb::MatchStatus::Completed as i32)
        .bind(score_a)
        .bind(score_b)
        .bind(winner)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_matches(&self, limit: u32) -> Result<Vec<pb::MatchSummary>> {
        let limit = if limit == 0 { 50 } else { limit };
        let rows = sqlx::query(
            "SELECT id, agent_a, agent_b, status, game_title, total_score_a, total_score_b,
                    winner, created_at_ms
             FROM matches ORDER BY created_at_ms DESC LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_summary).collect())
    }

    pub async fn get_match(&self, id: &str) -> Result<Option<(pb::MatchSummary, Option<String>)>> {
        let row = sqlx::query(
            "SELECT id, agent_a, agent_b, status, game_title, total_score_a, total_score_b,
                    winner, created_at_ms, spec_json
             FROM matches WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| {
            let spec_json: Option<String> = r.get("spec_json");
            (row_to_summary(r), spec_json)
        }))
    }

    // --- events ---

    /// Appends an event and returns its sequence number.
    pub async fn append_event(&self, match_id: &str, event: &pb::MatchEvent) -> Result<i64> {
        let json = serde_json::to_string(event)?;
        let row = sqlx::query(
            "INSERT INTO match_events (match_id, seq, event_json)
             VALUES (?, COALESCE((SELECT MAX(seq) + 1 FROM match_events WHERE match_id = ?), 0), ?)
             RETURNING seq",
        )
        .bind(match_id)
        .bind(match_id)
        .bind(json)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>("seq"))
    }

    pub async fn list_events(&self, match_id: &str) -> Result<Vec<(i64, pb::MatchEvent)>> {
        let rows = sqlx::query(
            "SELECT seq, event_json FROM match_events WHERE match_id = ? ORDER BY seq",
        )
        .bind(match_id)
        .fetch_all(&self.pool)
        .await?;
        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            let seq: i64 = row.get("seq");
            let event: pb::MatchEvent = serde_json::from_str(row.get("event_json"))?;
            events.push((seq, event));
        }
        Ok(events)
    }

    // --- rounds ---

    pub async fn save_round(&self, match_id: &str, result: &pb::RoundResult) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO rounds (match_id, round, result_json) VALUES (?, ?, ?)",
        )
        .bind(match_id)
        .bind(result.round as i64)
        .bind(serde_json::to_string(result)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_rounds(&self, match_id: &str) -> Result<Vec<pb::RoundResult>> {
        let rows = sqlx::query(
            "SELECT result_json FROM rounds WHERE match_id = ? ORDER BY round",
        )
        .bind(match_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| Ok(serde_json::from_str(r.get("result_json"))?))
            .collect()
    }

    // --- memos ---

    pub async fn latest_memo(&self, agent_id: &str) -> Result<Option<pb::Memo>> {
        let row = sqlx::query(
            "SELECT id, agent_id, version, content, match_id, created_at_ms
             FROM memos WHERE agent_id = ? ORDER BY version DESC LIMIT 1",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_to_memo))
    }

    pub async fn save_memo(&self, agent_id: &str, content: &str, match_id: &str) -> Result<pb::Memo> {
        let version: i64 = sqlx::query(
            "SELECT COALESCE(MAX(version) + 1, 1) AS v FROM memos WHERE agent_id = ?",
        )
        .bind(agent_id)
        .fetch_one(&self.pool)
        .await?
        .get("v");
        let memo = pb::Memo {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            version: version as u32,
            content: content.to_string(),
            match_id: match_id.to_string(),
            created_at_ms: now_ms(),
        };
        sqlx::query(
            "INSERT INTO memos (id, agent_id, version, content, match_id, created_at_ms)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&memo.id)
        .bind(&memo.agent_id)
        .bind(memo.version as i64)
        .bind(&memo.content)
        .bind(&memo.match_id)
        .bind(memo.created_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(memo)
    }

    pub async fn memo_lineage(&self, agent_id: &str) -> Result<Vec<pb::Memo>> {
        let rows = sqlx::query(
            "SELECT id, agent_id, version, content, match_id, created_at_ms
             FROM memos WHERE agent_id = ? ORDER BY version",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_memo).collect())
    }

    // --- stats ---

    pub async fn player_stats(&self, agent_id: &str) -> Result<pb::PlayerStats> {
        let row = sqlx::query(
            "SELECT
                COUNT(*) AS played,
                SUM(CASE WHEN winner = ? THEN 1 ELSE 0 END) AS wins,
                SUM(CASE WHEN winner != '' AND winner IS NOT NULL AND winner != ? THEN 1 ELSE 0 END) AS losses,
                SUM(CASE WHEN winner = '' THEN 1 ELSE 0 END) AS draws,
                SUM(CASE WHEN agent_a = ? THEN total_score_a ELSE total_score_b END) AS score
             FROM matches
             WHERE status = ? AND (agent_a = ? OR agent_b = ?)",
        )
        .bind(agent_id)
        .bind(agent_id)
        .bind(agent_id)
        .bind(pb::MatchStatus::Completed as i32)
        .bind(agent_id)
        .bind(agent_id)
        .fetch_one(&self.pool)
        .await?;
        let memo_version = self
            .latest_memo(agent_id)
            .await?
            .map(|m| m.version)
            .unwrap_or(0);
        Ok(pb::PlayerStats {
            agent_id: agent_id.to_string(),
            agent_name: String::new(),
            model: String::new(),
            matches_played: row.get::<i64, _>("played") as u32,
            wins: row.get::<Option<i64>, _>("wins").unwrap_or(0) as u32,
            losses: row.get::<Option<i64>, _>("losses").unwrap_or(0) as u32,
            draws: row.get::<Option<i64>, _>("draws").unwrap_or(0) as u32,
            total_score: row.get::<Option<f64>, _>("score").unwrap_or(0.0),
            memo_version,
        })
    }

    // --- meta ---

    pub async fn generation(&self) -> Result<u32> {
        let row = sqlx::query("SELECT value FROM meta WHERE key = 'generation'")
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .and_then(|r| r.get::<String, _>("value").parse().ok())
            .unwrap_or(0))
    }

    pub async fn set_generation(&self, generation: u32) -> Result<()> {
        sqlx::query("INSERT OR REPLACE INTO meta (key, value) VALUES ('generation', ?)")
            .bind(generation.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

fn row_to_summary(row: sqlx::sqlite::SqliteRow) -> pb::MatchSummary {
    let score_a: Option<f64> = row.get("total_score_a");
    let score_b: Option<f64> = row.get("total_score_b");
    let winner: Option<String> = row.get("winner");
    let result = score_a.map(|a| pb::MatchResult {
        total_score_a: a,
        total_score_b: score_b.unwrap_or(0.0),
        winner_agent_id: winner.unwrap_or_default(),
    });
    pb::MatchSummary {
        id: row.get("id"),
        game_title: row.get("game_title"),
        agent_a_id: row.get("agent_a"),
        agent_b_id: row.get("agent_b"),
        status: row.get::<i64, _>("status") as i32,
        result,
        created_at_ms: row.get("created_at_ms"),
    }
}

fn row_to_memo(row: sqlx::sqlite::SqliteRow) -> pb::Memo {
    pb::Memo {
        id: row.get("id"),
        agent_id: row.get("agent_id"),
        version: row.get::<i64, _>("version") as u32,
        content: row.get("content"),
        match_id: row.get("match_id"),
        created_at_ms: row.get("created_at_ms"),
    }
}
