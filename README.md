# Alice and Bob — LLM Game Theory Arena

Three LLMs, one loop:

- **Game Master (GM)**: invents a game as a structured spec (rules, turn structure, action schema, payoffs) and adjudicates anything ambiguous.
- **Alice & Bob (players)**: play the game over N rounds, each carrying a persistent **strategy memo** they revise after every match.
- **The loop is the experiment**: the GM sees past results and designs games that probe observed weaknesses; players' memos evolve to counter. No weights change — evolution lives in text. A GAN-flavored loop without gradient updates.

## Research questions

1. **Does text-based evolution work?** Win rate of memo-evolving players vs. frozen baselines of the same model.
2. **Convergence to game theory**: in repeated dilemma-style games, do memos converge to known equilibria (tit-for-tat, grim trigger, mixed strategies)?
3. **Adversarial curriculum**: does the GM produce progressively harder / more novel games when rewarded for discriminating between players?
4. **Cross-model dynamics**: fast/cheap models vs. frontier models — who exploits whom, and does cooperation emerge?
5. **Population dynamics**: with tournament selection over memo "genomes", do distinct strategic species emerge and persist?

## Architecture

- `proto/` — protobuf schemas (game spec, agents, arena service). Single source of truth for both sides.
- `server/` — Rust (axum + [connectrpc-axum](https://github.com/washanhanzi/connectrpc-axum)): match engine, GM orchestrator, population scheduler, provider-agnostic LLM client, SQLite persistence.
- `web/` — Vite + React + connect-web: dashboard/leaderboard, live match viewer (server-streaming), memo lineage inspector.

Any OpenAI-compatible provider works (OpenRouter, Groq, Ollama, ...). Models are mixed per role in config.

## Quick start

```sh
# 1. Configure providers and agents
cp providers.example.toml providers.toml
$EDITOR providers.toml   # add API keys / models

# 2. Run the server (fetches protoc automatically on first build)
cd server && cargo run

# 3. Run the web UI
cd web && npm install && npm run dev
```

Server listens on `http://localhost:3030`, web dev server on `http://localhost:5173`.

## Regenerating protobuf code

Rust code is generated automatically by `server/build.rs` on every build.

TypeScript clients are generated with buf (vendored via npm):

```sh
cd web && npm run gen
```

## Status

- [x] M0 — Scaffold: protos, codegen, server + web compile, RPC round trip
- [ ] M1 — One match, live: GM generates a game, Alice and Bob play it, events stream to the viewer
- [ ] M2 — Evolution: strategy memos, reflection, repeated series vs. frozen baseline
- [ ] M3 — Population: generations, tournament scheduler, leaderboard, GM curriculum memory
