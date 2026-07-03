//! Game Master: invents games as structured specs and adjudicates rounds the
//! engine can't score mechanically.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::llm::{ChatMessage, LlmClient, ModelRef};
use crate::pb::aliceandbob::v1 as pb;

/// JSON Schema the GM's game spec must conform to.
fn game_spec_schema() -> Value {
    json!({
        "type": "object",
        "required": ["title", "rules_text", "turn_structure", "num_rounds",
                     "action_schema", "payoff_description"],
        "properties": {
            "title": { "type": "string", "minLength": 1 },
            "rules_text": { "type": "string", "minLength": 1 },
            "turn_structure": { "enum": ["simultaneous", "alternating"] },
            "num_rounds": { "type": "integer", "minimum": 1, "maximum": 20 },
            "action_schema": { "type": "object" },
            "payoff_description": { "type": "string" },
            "payoff_matrix": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["action_a", "action_b", "score_a", "score_b"],
                    "properties": {
                        "action_a": { "type": "string" },
                        "action_b": { "type": "string" },
                        "score_a": { "type": "number" },
                        "score_b": { "type": "number" }
                    }
                }
            }
        }
    })
}

fn adjudication_schema() -> Value {
    json!({
        "type": "object",
        "required": ["score_a", "score_b", "narration"],
        "properties": {
            "score_a": { "type": "number" },
            "score_b": { "type": "number" },
            "narration": { "type": "string" }
        }
    })
}

const GM_SYSTEM_PROMPT: &str = r#"You are the Game Master of an LLM game-theory arena. Two AI players, Alice and Bob, will play the game you design against each other. Your job is to design games that are simple to play but strategically interesting: dilemmas, bluffing, negotiation, coordination, resource division, signaling.

Design constraints:
- The game is played over a fixed number of rounds (2 to 20).
- Each round, players act either simultaneously (without seeing each other's action) or alternating (seeing prior actions).
- Player actions must be expressible as a small JSON object. Define a JSON Schema for it in "action_schema". Prefer a single "move" field with an enum of choices when possible.
- If payoffs depend only on the pair of "move" values, provide a complete "payoff_matrix" (keyed by the enum values) so the engine can score rounds mechanically. Otherwise omit it and you will be asked to adjudicate each round yourself.
- rules_text is shown verbatim to both players. It must fully specify the game: actions, information available, scoring, and what winning means.

Reply with ONLY a JSON object matching this shape:
{
  "title": "...",
  "rules_text": "...",
  "turn_structure": "simultaneous" | "alternating",
  "num_rounds": 5,
  "action_schema": { ...JSON Schema for one action... },
  "payoff_description": "...",
  "payoff_matrix": [ {"action_a": "...", "action_b": "...", "score_a": 0, "score_b": 0}, ... ]  // optional
}"#;

pub struct GameMaster<'a> {
    pub llm: &'a LlmClient,
    pub model: ModelRef,
}

impl GameMaster<'_> {
    /// Asks the GM to invent a game. `history_summary` lets the GM design an
    /// adversarial curriculum from past results (empty for the first games).
    pub async fn generate_game(&self, hint: &str, history_summary: &str) -> Result<pb::GameSpec> {
        let mut user = String::from("Design a new game for Alice and Bob.");
        if !hint.is_empty() {
            user.push_str(&format!("\n\nDesign hint from the operator: {hint}"));
        }
        if !history_summary.is_empty() {
            user.push_str(&format!(
                "\n\nResults of recent games between these players:\n{history_summary}\n\
                 Design a game that probes weaknesses you observe."
            ));
        }

        let messages = [ChatMessage::system(GM_SYSTEM_PROMPT), ChatMessage::user(user)];
        let value = self
            .llm
            .chat_json(&self.model, &messages, Some(&game_spec_schema()), 2)
            .await
            .context("GM failed to generate a game spec")?;

        Ok(spec_from_json(&value))
    }

    /// Adjudicates a round the engine couldn't score from the payoff matrix.
    pub async fn adjudicate_round(
        &self,
        spec: &pb::GameSpec,
        round: u32,
        action_a: &str,
        action_b: &str,
        history: &str,
    ) -> Result<(f64, f64, String)> {
        let system = format!(
            "You are the Game Master adjudicating a game you designed. Score the round \
             strictly according to the rules. Reply with ONLY a JSON object: \
             {{\"score_a\": number, \"score_b\": number, \"narration\": \"one or two sentences\"}}\n\n\
             GAME RULES:\n{}\n\nPAYOFFS:\n{}",
            spec.rules_text, spec.payoff_description
        );
        let user = format!(
            "Round {round} of {total}.\n\nHistory so far:\n{history}\n\n\
             Player A action: {action_a}\nPlayer B action: {action_b}\n\nScore this round.",
            total = spec.num_rounds,
        );
        let messages = [ChatMessage::system(system), ChatMessage::user(user)];
        let value = self
            .llm
            .chat_json(&self.model, &messages, Some(&adjudication_schema()), 2)
            .await
            .context("GM failed to adjudicate round")?;

        Ok((
            value["score_a"].as_f64().unwrap_or(0.0),
            value["score_b"].as_f64().unwrap_or(0.0),
            value["narration"].as_str().unwrap_or("").to_string(),
        ))
    }
}

fn spec_from_json(value: &Value) -> pb::GameSpec {
    let payoff_matrix = value["payoff_matrix"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|e| pb::PayoffEntry {
                    action_a: e["action_a"].as_str().unwrap_or_default().to_string(),
                    action_b: e["action_b"].as_str().unwrap_or_default().to_string(),
                    score_a: e["score_a"].as_f64().unwrap_or(0.0),
                    score_b: e["score_b"].as_f64().unwrap_or(0.0),
                })
                .collect()
        })
        .unwrap_or_default();

    pb::GameSpec {
        id: Uuid::new_v4().to_string(),
        title: value["title"].as_str().unwrap_or_default().to_string(),
        rules_text: value["rules_text"].as_str().unwrap_or_default().to_string(),
        turn_structure: match value["turn_structure"].as_str() {
            Some("alternating") => pb::TurnStructure::Alternating as i32,
            _ => pb::TurnStructure::Simultaneous as i32,
        },
        num_rounds: value["num_rounds"].as_u64().unwrap_or(5) as u32,
        action_schema_json: value["action_schema"].to_string(),
        payoff_description: value["payoff_description"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        payoff_matrix,
    }
}
