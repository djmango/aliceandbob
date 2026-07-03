//! Population layer (M3 groundwork): leaderboard stats and a naive
//! round-robin generation scheduler. Selection/mutation of memos comes later.

use anyhow::Result;
use uuid::Uuid;

use crate::engine::Engine;
use crate::pb::aliceandbob::v1 as pb;

pub async fn leaderboard(engine: &Engine) -> Result<Vec<pb::PlayerStats>> {
    let mut stats = Vec::new();
    for agent in engine.config.players() {
        let mut s = engine.store.player_stats(&agent.id).await?;
        s.agent_name = agent.name.clone();
        s.model = agent.model.clone();
        stats.push(s);
    }
    stats.sort_by(|a, b| {
        b.total_score
            .partial_cmp(&a.total_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(stats)
}

/// Starts one generation: every distinct pair of players meets
/// `matches_per_pairing` times. Matches run concurrently as background tasks.
pub async fn start_generation(
    engine: &Engine,
    matches_per_pairing: u32,
) -> Result<(u32, Vec<String>)> {
    let generation = engine.store.generation().await? + 1;
    engine.store.set_generation(generation).await?;

    let players: Vec<_> = engine.config.players().collect();
    let gm = engine
        .config
        .default_gm()
        .ok_or_else(|| anyhow::anyhow!("no game_master agent configured"))?;
    let per_pairing = matches_per_pairing.max(1);

    let mut match_ids = Vec::new();
    for i in 0..players.len() {
        for j in (i + 1)..players.len() {
            for _ in 0..per_pairing {
                let match_id = Uuid::new_v4().to_string();
                engine
                    .store
                    .create_match(&match_id, &players[i].id, &players[j].id, &gm.id)
                    .await?;
                tokio::spawn(engine.clone().run_match(
                    match_id.clone(),
                    players[i].id.clone(),
                    players[j].id.clone(),
                    gm.id.clone(),
                    String::new(),
                ));
                match_ids.push(match_id);
            }
        }
    }
    Ok((generation, match_ids))
}
