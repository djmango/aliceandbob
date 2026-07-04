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

Any OpenAI-compatible provider works. Models are mixed per role in config, and each provider gets its own rate-limit queue (paced starts + bounded concurrency + 429 backoff), so everything runs comfortably on free tiers.

## Free providers

| Provider | Free tier (mid-2026) | Get a key |
|---|---|---|
| **Groq** (default) | ~30 req/min, ~1k req/day, very fast | [console.groq.com/keys](https://console.groq.com/keys) |
| **OpenRouter** | 20+ `:free` models, ~50 req/day (1k/day after $10 top-up) | [openrouter.ai/settings/keys](https://openrouter.ai/settings/keys) |
| **Google AI Studio** | Gemini Flash, up to ~1.5k req/day | [aistudio.google.com/apikey](https://aistudio.google.com/apikey) |
| **Cerebras** | ~1M tokens/day at ~2k tok/s | [cloud.cerebras.ai](https://cloud.cerebras.ai/) |
| **Mistral** | free Experiment tier | [console.mistral.ai/api-keys](https://console.mistral.ai/api-keys) |
| **GitHub Models** | daily limits, uses a PAT with `models` scope | [github.com/settings/tokens](https://github.com/settings/tokens) |
| **Ollama** | local GPU, unlimited | — |

None of these require a credit card. See `providers.example.toml` for ready-made config blocks with sensible `requests_per_minute` values.

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

## Docker / homelab

CI publishes `ghcr.io/djmango/aliceandbob:latest` (single container: Rust server + built UI). Run it anywhere:

```sh
docker run -p 3030:3030 \
  -v ./providers.toml:/app/providers.toml:ro \
  -v aliceandbob-data:/data \
  -e GROQ_API_KEY=... \
  ghcr.io/djmango/aliceandbob:latest
```

Set `web_dist = "/app/web-dist"` and `database = "/data/aliceandbob.sqlite"` in the mounted `providers.toml`. The UI is served on the same port as the API.

## Status

- [x] M0 — Scaffold: protos, codegen, server + web compile, RPC round trip
- [ ] M1 — One match, live: GM generates a game, Alice and Bob play it, events stream to the viewer
- [ ] M2 — Evolution: strategy memos, reflection, repeated series vs. frozen baseline
- [ ] M3 — Population: generations, tournament scheduler, leaderboard, GM curriculum memory
