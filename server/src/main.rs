mod config;
mod engine;
mod gm;
mod llm;
mod population;
mod store;

mod pb {
    include!(concat!(env!("OUT_DIR"), "/protos.rs"));
}

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use connectrpc_axum::prelude::*;
use futures::Stream;
use tower_http::cors::CorsLayer;
use uuid::Uuid;

use crate::config::Config;
use crate::engine::{Engine, EventBus};
use crate::llm::LlmClient;
use crate::pb::aliceandbob::v1 as pbv1;
use crate::pb::aliceandbob::v1::arena_service_connect::ArenaServiceBuilder;
use crate::store::Store;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,aliceandbob_server=debug".into()),
        )
        .init();

    let config = Arc::new(Config::load()?);
    let store = Store::open(&config.server.database).await?;
    let interrupted = store.fail_interrupted_matches().await?;
    if !interrupted.is_empty() {
        tracing::warn!(
            count = interrupted.len(),
            "marked interrupted matches as failed after restart"
        );
    }
    let engine = Engine {
        config: config.clone(),
        llm: LlmClient::new(),
        store,
        bus: EventBus::default(),
    };

    let connect_router = ArenaServiceBuilder::new()
        .start_match(start_match)
        .watch_match(watch_match)
        .list_matches(list_matches)
        .get_match(get_match)
        .list_agents(list_agents)
        .get_population(get_population)
        .get_memo_lineage(get_memo_lineage)
        .start_generation(start_generation)
        .with_state(engine)
        .build_connect();

    let mut app = connect_router.route("/health", get(|| async { "ok" }));

    // Serve the built web UI (SPA fallback to index.html) when configured,
    // so one process hosts both API and frontend in deployment.
    if let Some(dist) = &config.server.web_dist {
        if std::path::Path::new(dist).is_dir() {
            let index = format!("{dist}/index.html");
            app = app.fallback_service(
                tower_http::services::ServeDir::new(dist)
                    .fallback(tower_http::services::ServeFile::new(index)),
            );
            tracing::info!(dist, "serving web UI");
        } else {
            tracing::warn!(dist, "web_dist directory not found; UI not served");
        }
    }

    let app = app.layer(CorsLayer::very_permissive());

    let addr = format!("0.0.0.0:{}", config.server.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn start_match(
    State(engine): State<Engine>,
    ConnectRequest(req): ConnectRequest<pbv1::StartMatchRequest>,
) -> Result<ConnectResponse<pbv1::StartMatchResponse>, ConnectError> {
    let config = &engine.config;
    for id in [&req.agent_a_id, &req.agent_b_id] {
        if config.agent(id).is_none() {
            return Err(ConnectError::new_invalid_argument(format!(
                "unknown agent '{id}'"
            )));
        }
    }
    let gm_id = if req.gm_agent_id.is_empty() {
        config
            .default_gm()
            .map(|a| a.id.clone())
            .ok_or_else(|| ConnectError::new_failed_precondition("no game_master agent configured"))?
    } else {
        req.gm_agent_id.clone()
    };

    let match_id = Uuid::new_v4().to_string();
    engine
        .store
        .create_match(&match_id, &req.agent_a_id, &req.agent_b_id, &gm_id)
        .await
        .map_err(internal)?;
    // Open the event bus before spawning so WatchMatch never races an empty channel.
    engine.bus.open(&match_id).await;

    tokio::spawn(engine.clone().run_match(
        match_id.clone(),
        req.agent_a_id,
        req.agent_b_id,
        gm_id,
        req.game_hint,
    ));

    Ok(ConnectResponse::new(pbv1::StartMatchResponse { match_id }))
}

async fn watch_match(
    State(engine): State<Engine>,
    ConnectRequest(req): ConnectRequest<pbv1::WatchMatchRequest>,
) -> Result<
    ConnectResponse<StreamBody<impl Stream<Item = Result<pbv1::MatchEvent, ConnectError>>>>,
    ConnectError,
> {
    let match_id = req.match_id;
    if engine
        .store
        .get_match(&match_id)
        .await
        .map_err(internal)?
        .is_none()
    {
        return Err(ConnectError::new_not_found(format!(
            "match '{match_id}' not found"
        )));
    }

    // Subscribe first, then replay persisted events, deduping by sequence
    // number, so no event is dropped between replay and live tail.
    let live = engine.bus.subscribe(&match_id).await;
    let persisted = engine.store.list_events(&match_id).await.map_err(internal)?;

    let stream = async_stream::stream! {
        let mut last_seq: i64 = -1;
        for (seq, event) in persisted {
            last_seq = seq;
            yield Ok(event);
        }
        if let Some(mut rx) = live {
            loop {
                match rx.recv().await {
                    Ok((seq, event)) => {
                        if seq > last_seq {
                            last_seq = seq;
                            yield Ok(event);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        } else if let Some((summary, _)) = engine.store.get_match(&match_id).await.ok().flatten() {
            let status = pbv1::MatchStatus::try_from(summary.status)
                .unwrap_or(pbv1::MatchStatus::Unspecified);
            if Store::is_active_status(status) {
                let message = "Match is no longer running (server restarted or the task ended).";
                let _ = engine
                    .store
                    .fail_match_with_error(&match_id, message)
                    .await;
                yield Ok(pbv1::MatchEvent {
                    match_id: match_id.clone(),
                    timestamp_ms: crate::store::now_ms(),
                    event: Some(pbv1::match_event::Event::MatchError(
                        pbv1::match_event::MatchError {
                            message: message.to_string(),
                        },
                    )),
                });
            }
        }
    };
    Ok(ConnectResponse::new(StreamBody::new(stream)))
}

async fn list_matches(
    State(engine): State<Engine>,
    ConnectRequest(req): ConnectRequest<pbv1::ListMatchesRequest>,
) -> Result<ConnectResponse<pbv1::ListMatchesResponse>, ConnectError> {
    let matches = engine.store.list_matches(req.limit).await.map_err(internal)?;
    Ok(ConnectResponse::new(pbv1::ListMatchesResponse { matches }))
}

async fn get_match(
    State(engine): State<Engine>,
    ConnectRequest(req): ConnectRequest<pbv1::GetMatchRequest>,
) -> Result<ConnectResponse<pbv1::GetMatchResponse>, ConnectError> {
    let Some((summary, spec_json)) = engine
        .store
        .get_match(&req.match_id)
        .await
        .map_err(internal)?
    else {
        return Err(ConnectError::new_not_found(format!(
            "match '{}' not found",
            req.match_id
        )));
    };
    let spec = spec_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(internal)?;
    let rounds = engine
        .store
        .list_rounds(&req.match_id)
        .await
        .map_err(internal)?;
    Ok(ConnectResponse::new(pbv1::GetMatchResponse {
        summary: Some(summary),
        spec,
        rounds,
    }))
}

async fn list_agents(
    State(engine): State<Engine>,
    ConnectRequest(_req): ConnectRequest<pbv1::ListAgentsRequest>,
) -> Result<ConnectResponse<pbv1::ListAgentsResponse>, ConnectError> {
    let agents = engine.config.agents.iter().map(|a| a.to_proto()).collect();
    Ok(ConnectResponse::new(pbv1::ListAgentsResponse { agents }))
}

async fn get_population(
    State(engine): State<Engine>,
    ConnectRequest(_req): ConnectRequest<pbv1::GetPopulationRequest>,
) -> Result<ConnectResponse<pbv1::GetPopulationResponse>, ConnectError> {
    let players = population::leaderboard(&engine).await.map_err(internal)?;
    let current_generation = engine.store.generation().await.map_err(internal)?;
    Ok(ConnectResponse::new(pbv1::GetPopulationResponse {
        players,
        current_generation,
    }))
}

async fn get_memo_lineage(
    State(engine): State<Engine>,
    ConnectRequest(req): ConnectRequest<pbv1::GetMemoLineageRequest>,
) -> Result<ConnectResponse<pbv1::GetMemoLineageResponse>, ConnectError> {
    let memos = engine
        .store
        .memo_lineage(&req.agent_id)
        .await
        .map_err(internal)?;
    Ok(ConnectResponse::new(pbv1::GetMemoLineageResponse { memos }))
}

async fn start_generation(
    State(engine): State<Engine>,
    ConnectRequest(req): ConnectRequest<pbv1::StartGenerationRequest>,
) -> Result<ConnectResponse<pbv1::StartGenerationResponse>, ConnectError> {
    let (generation, match_ids) =
        population::start_generation(&engine, req.matches_per_pairing)
            .await
            .map_err(internal)?;
    Ok(ConnectResponse::new(pbv1::StartGenerationResponse {
        generation,
        match_ids,
    }))
}

fn internal(e: impl std::fmt::Display) -> ConnectError {
    ConnectError::new_internal(e.to_string())
}
