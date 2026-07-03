//! Match engine: runs one full match between two players, streaming events.
//!
//! Flow: GM generates a GameSpec -> players act each round (simultaneous or
//! alternating) -> engine scores from the payoff matrix when it can, otherwise
//! the GM adjudicates -> after the final round each player reflects and
//! rewrites its strategy memo.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::sync::{broadcast, RwLock};

use crate::config::Config;
use crate::gm::GameMaster;
use crate::llm::{ChatMessage, LlmClient, ModelRef};
use crate::pb::aliceandbob::v1 as pb;
use crate::store::{now_ms, Store};

/// Per-match broadcast channels feeding WatchMatch streams.
#[derive(Clone, Default)]
pub struct EventBus {
    channels: Arc<RwLock<HashMap<String, broadcast::Sender<(i64, pb::MatchEvent)>>>>,
}

impl EventBus {
    pub async fn open(&self, match_id: &str) -> broadcast::Sender<(i64, pb::MatchEvent)> {
        let mut channels = self.channels.write().await;
        channels
            .entry(match_id.to_string())
            .or_insert_with(|| broadcast::channel(256).0)
            .clone()
    }

    pub async fn subscribe(
        &self,
        match_id: &str,
    ) -> Option<broadcast::Receiver<(i64, pb::MatchEvent)>> {
        let channels = self.channels.read().await;
        channels.get(match_id).map(|tx| tx.subscribe())
    }

    /// Drops the channel; subscribers see Closed and end their streams.
    pub async fn close(&self, match_id: &str) {
        self.channels.write().await.remove(match_id);
    }
}

#[derive(Clone)]
pub struct Engine {
    pub config: Arc<Config>,
    pub llm: LlmClient,
    pub store: Store,
    pub bus: EventBus,
}

struct MatchContext {
    match_id: String,
    tx: broadcast::Sender<(i64, pb::MatchEvent)>,
}

impl Engine {
    /// Persists an event and fans it out to live watchers.
    async fn emit(&self, ctx: &MatchContext, event: pb::match_event::Event) -> Result<()> {
        let event = pb::MatchEvent {
            match_id: ctx.match_id.clone(),
            timestamp_ms: now_ms(),
            event: Some(event),
        };
        let seq = self.store.append_event(&ctx.match_id, &event).await?;
        let _ = ctx.tx.send((seq, event)); // no live watchers is fine
        Ok(())
    }

    /// Runs a match to completion. Spawned as a background task; all progress
    /// is observable through the event stream and the store.
    pub async fn run_match(
        self,
        match_id: String,
        agent_a_id: String,
        agent_b_id: String,
        gm_agent_id: String,
        game_hint: String,
    ) {
        let tx = self.bus.open(&match_id).await;
        let ctx = MatchContext { match_id: match_id.clone(), tx };

        let outcome = self
            .run_match_inner(&ctx, &agent_a_id, &agent_b_id, &gm_agent_id, &game_hint)
            .await;

        if let Err(e) = outcome {
            tracing::error!(match_id, error = %e, "match failed");
            let _ = self
                .store
                .set_match_status(&match_id, pb::MatchStatus::Failed)
                .await;
            let _ = self
                .emit(
                    &ctx,
                    pb::match_event::Event::MatchError(pb::match_event::MatchError {
                        message: e.to_string(),
                    }),
                )
                .await;
        }
        self.bus.close(&match_id).await;
    }

    async fn run_match_inner(
        &self,
        ctx: &MatchContext,
        agent_a_id: &str,
        agent_b_id: &str,
        gm_agent_id: &str,
        game_hint: &str,
    ) -> Result<()> {
        let match_id = &ctx.match_id;

        // --- 1. GM invents the game ---
        self.store
            .set_match_status(match_id, pb::MatchStatus::GeneratingGame)
            .await?;
        let gm = GameMaster {
            llm: &self.llm,
            model: ModelRef::for_agent(&self.config, gm_agent_id)?,
        };
        let spec = gm.generate_game(game_hint, "").await?;
        self.store
            .set_match_spec(match_id, &spec.title, &serde_json::to_string(&spec)?)
            .await?;
        self.emit(
            ctx,
            pb::match_event::Event::GameGenerated(pb::match_event::GameGenerated {
                spec: Some(spec.clone()),
            }),
        )
        .await?;

        // --- 2. Play rounds ---
        self.store
            .set_match_status(match_id, pb::MatchStatus::InProgress)
            .await?;

        let action_schema: Value = serde_json::from_str(&spec.action_schema_json)
            .context("GM produced an unparseable action schema")?;
        let model_a = ModelRef::for_agent(&self.config, agent_a_id)?;
        let model_b = ModelRef::for_agent(&self.config, agent_b_id)?;
        let memo_a = self.latest_memo_content(agent_a_id).await?;
        let memo_b = self.latest_memo_content(agent_b_id).await?;

        let mut history: Vec<pb::RoundResult> = Vec::new();
        let (mut total_a, mut total_b) = (0.0_f64, 0.0_f64);
        let simultaneous = spec.turn_structure != pb::TurnStructure::Alternating as i32;

        for round in 1..=spec.num_rounds {
            self.emit(
                ctx,
                pb::match_event::Event::RoundStarted(pb::match_event::RoundStarted { round }),
            )
            .await?;

            let history_text = render_history(&history, agent_a_id, agent_b_id);

            let (action_a, action_b) = if simultaneous {
                let (a, b) = tokio::join!(
                    self.player_action(
                        &model_a, agent_a_id, &spec, &action_schema, round, &history_text,
                        &memo_a, None
                    ),
                    self.player_action(
                        &model_b, agent_b_id, &spec, &action_schema, round, &history_text,
                        &memo_b, None
                    ),
                );
                (a?, b?)
            } else {
                let a = self
                    .player_action(
                        &model_a, agent_a_id, &spec, &action_schema, round, &history_text,
                        &memo_a, None,
                    )
                    .await?;
                self.emit(
                    ctx,
                    pb::match_event::Event::ActionSubmitted(pb::match_event::ActionSubmitted {
                        action: Some(a.clone()),
                    }),
                )
                .await?;
                let b = self
                    .player_action(
                        &model_b, agent_b_id, &spec, &action_schema, round, &history_text,
                        &memo_b, Some(&a.action_json),
                    )
                    .await?;
                (a, b)
            };

            if simultaneous {
                self.emit(
                    ctx,
                    pb::match_event::Event::ActionSubmitted(pb::match_event::ActionSubmitted {
                        action: Some(action_a.clone()),
                    }),
                )
                .await?;
            }
            self.emit(
                ctx,
                pb::match_event::Event::ActionSubmitted(pb::match_event::ActionSubmitted {
                    action: Some(action_b.clone()),
                }),
            )
            .await?;

            // --- 3. Score: engine if possible, GM otherwise ---
            let (score_a, score_b, narration, adjudicator) = match score_from_matrix(
                &spec,
                &action_a.action_json,
                &action_b.action_json,
            ) {
                Some((sa, sb)) => (sa, sb, String::new(), pb::Adjudicator::Engine),
                None => {
                    let (sa, sb, narration) = gm
                        .adjudicate_round(
                            &spec,
                            round,
                            &action_a.action_json,
                            &action_b.action_json,
                            &render_history(&history, agent_a_id, agent_b_id),
                        )
                        .await?;
                    (sa, sb, narration, pb::Adjudicator::Gm)
                }
            };
            total_a += score_a;
            total_b += score_b;

            let result = pb::RoundResult {
                round,
                actions: vec![action_a, action_b],
                score_a,
                score_b,
                narration,
                adjudicated_by: adjudicator as i32,
            };
            self.store.save_round(match_id, &result).await?;
            self.emit(
                ctx,
                pb::match_event::Event::RoundScored(pb::match_event::RoundScored {
                    result: Some(result.clone()),
                }),
            )
            .await?;
            history.push(result);
        }

        // --- 4. Reflection: players rewrite their strategy memos ---
        self.store
            .set_match_status(match_id, pb::MatchStatus::Reflecting)
            .await?;
        let final_history = render_history(&history, agent_a_id, agent_b_id);
        for (agent_id, model, memo, own_total, opp_total) in [
            (agent_a_id, &model_a, &memo_a, total_a, total_b),
            (agent_b_id, &model_b, &memo_b, total_b, total_a),
        ] {
            match self
                .reflect(model, &spec, &final_history, memo, own_total, opp_total)
                .await
            {
                Ok(new_memo) => {
                    let saved = self.store.save_memo(agent_id, &new_memo, match_id).await?;
                    self.emit(
                        ctx,
                        pb::match_event::Event::MemoUpdated(pb::match_event::MemoUpdated {
                            agent_id: agent_id.to_string(),
                            memo_version: saved.version,
                        }),
                    )
                    .await?;
                }
                // A failed reflection shouldn't void the match result.
                Err(e) => tracing::warn!(agent_id, error = %e, "reflection failed"),
            }
        }

        // --- 5. Finish ---
        let winner = if total_a > total_b {
            agent_a_id
        } else if total_b > total_a {
            agent_b_id
        } else {
            ""
        };
        self.store
            .finish_match(match_id, total_a, total_b, winner)
            .await?;
        self.emit(
            ctx,
            pb::match_event::Event::MatchCompleted(pb::match_event::MatchCompleted {
                result: Some(pb::MatchResult {
                    total_score_a: total_a,
                    total_score_b: total_b,
                    winner_agent_id: winner.to_string(),
                }),
            }),
        )
        .await?;
        Ok(())
    }

    async fn latest_memo_content(&self, agent_id: &str) -> Result<String> {
        Ok(self
            .store
            .latest_memo(agent_id)
            .await?
            .map(|m| m.content)
            .unwrap_or_default())
    }

    #[allow(clippy::too_many_arguments)]
    async fn player_action(
        &self,
        model: &ModelRef,
        agent_id: &str,
        spec: &pb::GameSpec,
        action_schema: &Value,
        round: u32,
        history: &str,
        memo: &str,
        opponent_action_this_round: Option<&str>,
    ) -> Result<pb::Action> {
        let persona = self
            .config
            .agent(agent_id)
            .map(|a| a.persona.clone())
            .unwrap_or_default();

        let mut system = format!(
            "You are {agent_id}, a player in a competitive game arena. Play to maximize your \
             own total score across all rounds.\n\nGAME: {title}\n\nRULES:\n{rules}\n\n\
             PAYOFFS:\n{payoffs}",
            title = spec.title,
            rules = spec.rules_text,
            payoffs = spec.payoff_description,
        );
        if !persona.is_empty() {
            system.push_str(&format!("\n\nYOUR PERSONA:\n{persona}"));
        }
        if !memo.is_empty() {
            system.push_str(&format!(
                "\n\nYOUR PRIVATE STRATEGY MEMO (from past games, never shown to the opponent):\n{memo}"
            ));
        }
        system.push_str(&format!(
            "\n\nReply with ONLY a JSON object of the form:\n\
             {{\"reasoning\": \"your private thinking, never shown to the opponent\", \
             \"action\": <object matching the action schema>}}\n\n\
             ACTION SCHEMA:\n{action_schema}"
        ));

        let mut user = format!(
            "Round {round} of {total}.\n\nHistory so far:\n{history}",
            total = spec.num_rounds,
            history = if history.is_empty() { "(first round)" } else { history },
        );
        if let Some(opp) = opponent_action_this_round {
            user.push_str(&format!(
                "\n\nYour opponent has already acted this round: {opp}"
            ));
        }
        user.push_str("\n\nSubmit your action.");

        let reply_schema = json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "reasoning": { "type": "string" },
                "action": action_schema,
            }
        });

        let messages = [ChatMessage::system(system), ChatMessage::user(user)];
        let value = self
            .llm
            .chat_json(model, &messages, Some(&reply_schema), 2)
            .await
            .with_context(|| format!("player {agent_id} failed to act"))?;

        Ok(pb::Action {
            agent_id: agent_id.to_string(),
            round,
            action_json: value["action"].to_string(),
            private_reasoning: value["reasoning"].as_str().unwrap_or("").to_string(),
        })
    }

    async fn reflect(
        &self,
        model: &ModelRef,
        spec: &pb::GameSpec,
        history: &str,
        old_memo: &str,
        own_total: f64,
        opp_total: f64,
    ) -> Result<String> {
        let system = "You are a game player improving between matches. Given the match that \
             just ended and your current strategy memo, write an improved memo: concrete, \
             transferable strategic principles for future games against this opponent and \
             others. Keep it under 400 words. Reply with ONLY a JSON object: \
             {\"reflection\": \"what happened and why\", \"memo\": \"the full new memo text\"}";
        let user = format!(
            "GAME: {title}\n\nRULES:\n{rules}\n\nFULL MATCH HISTORY:\n{history}\n\n\
             FINAL SCORE: you {own_total} vs opponent {opp_total}\n\n\
             YOUR CURRENT MEMO:\n{memo}\n\nWrite your reflection and revised memo.",
            title = spec.title,
            rules = spec.rules_text,
            memo = if old_memo.is_empty() { "(none yet)" } else { old_memo },
        );
        let schema = json!({
            "type": "object",
            "required": ["memo"],
            "properties": {
                "reflection": { "type": "string" },
                "memo": { "type": "string", "minLength": 1 }
            }
        });
        let messages = [ChatMessage::system(system.to_string()), ChatMessage::user(user)];
        let value = self.llm.chat_json(model, &messages, Some(&schema), 2).await?;
        value["memo"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("reflection reply missing memo"))
    }
}

/// Mechanical scoring: works when the spec has a payoff matrix and both
/// actions carry a string "move" field matching matrix keys.
fn score_from_matrix(
    spec: &pb::GameSpec,
    action_a_json: &str,
    action_b_json: &str,
) -> Option<(f64, f64)> {
    if spec.payoff_matrix.is_empty() {
        return None;
    }
    let move_of = |raw: &str| -> Option<String> {
        let v: Value = serde_json::from_str(raw).ok()?;
        v.get("move")?.as_str().map(|s| s.to_string())
    };
    let (move_a, move_b) = (move_of(action_a_json)?, move_of(action_b_json)?);
    spec.payoff_matrix
        .iter()
        .find(|e| e.action_a == move_a && e.action_b == move_b)
        .map(|e| (e.score_a, e.score_b))
}

fn render_history(history: &[pb::RoundResult], agent_a_id: &str, agent_b_id: &str) -> String {
    history
        .iter()
        .map(|r| {
            let actions: Vec<String> = r
                .actions
                .iter()
                .map(|a| format!("{}: {}", a.agent_id, a.action_json))
                .collect();
            let narration = if r.narration.is_empty() {
                String::new()
            } else {
                format!(" — {}", r.narration)
            };
            format!(
                "Round {}: {} | scores: {} {:+.1}, {} {:+.1}{}",
                r.round,
                actions.join(", "),
                agent_a_id,
                r.score_a,
                agent_b_id,
                r.score_b,
                narration,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
