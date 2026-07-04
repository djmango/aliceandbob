import { useCallback, useEffect, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { arena } from "../api";
import type { AgentConfig, PlayerStats } from "../gen/aliceandbob/v1/agents_pb";
import type { MatchSummary } from "../gen/aliceandbob/v1/game_pb";
import { MatchStatus } from "../gen/aliceandbob/v1/game_pb";
import { agentLabel, isPlayer } from "../agents";

const STATUS_LABELS: Record<MatchStatus, string> = {
  [MatchStatus.UNSPECIFIED]: "unknown",
  [MatchStatus.PENDING]: "pending",
  [MatchStatus.GENERATING_GAME]: "designing game",
  [MatchStatus.IN_PROGRESS]: "in progress",
  [MatchStatus.REFLECTING]: "reflecting",
  [MatchStatus.COMPLETED]: "completed",
  [MatchStatus.FAILED]: "failed",
};

export default function Dashboard() {
  const navigate = useNavigate();
  const [agents, setAgents] = useState<AgentConfig[]>([]);
  const [players, setPlayers] = useState<PlayerStats[]>([]);
  const [matches, setMatches] = useState<MatchSummary[]>([]);
  const [generation, setGeneration] = useState(0);
  const [agentA, setAgentA] = useState("");
  const [agentB, setAgentB] = useState("");
  const [hint, setHint] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const refresh = useCallback(async () => {
    try {
      const [population, matchList] = await Promise.all([
        arena.getPopulation({}),
        arena.listMatches({ limit: 25 }),
      ]);
      setPlayers(population.players);
      setGeneration(population.currentGeneration);
      setMatches(matchList.matches);
      setError("");
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    arena
      .listAgents({})
      .then((r) => {
        setAgents(r.agents);
        const playerAgents = r.agents.filter(isPlayer);
        if (playerAgents.length >= 2) {
          setAgentA(playerAgents[0].id);
          setAgentB(playerAgents[1].id);
        }
      })
      .catch((e) => setError(String(e)));
    refresh();
    const timer = setInterval(refresh, 4000);
    return () => clearInterval(timer);
  }, [refresh]);

  const startMatch = async () => {
    setBusy(true);
    try {
      const r = await arena.startMatch({
        agentAId: agentA,
        agentBId: agentB,
        gameHint: hint,
      });
      navigate(`/match/${r.matchId}`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const playerAgents = agents.filter(isPlayer);

  return (
    <div className="page">
      {error && <div className="error-banner">{error}</div>}

      <section className="card">
        <h2>New Match</h2>
        <div className="new-match-row">
          <label>
            Player A
            <select value={agentA} onChange={(e) => setAgentA(e.target.value)}>
              {playerAgents.map((a) => (
                <option key={a.id} value={a.id}>
                  {agentLabel(a)}
                </option>
              ))}
            </select>
          </label>
          <span className="vs">vs</span>
          <label>
            Player B
            <select value={agentB} onChange={(e) => setAgentB(e.target.value)}>
              {playerAgents.map((a) => (
                <option key={a.id} value={a.id}>
                  {agentLabel(a)}
                </option>
              ))}
            </select>
          </label>
          <label className="hint-field">
            Game hint (optional)
            <input
              value={hint}
              onChange={(e) => setHint(e.target.value)}
              placeholder='e.g. "design a bluffing game"'
            />
          </label>
          <button disabled={busy || !agentA || !agentB || agentA === agentB} onClick={startMatch}>
            {busy ? "Starting..." : "Start Match"}
          </button>
        </div>
      </section>

      <div className="two-col">
        <section className="card">
          <h2>
            Leaderboard <span className="muted">generation {generation}</span>
          </h2>
          <table>
            <thead>
              <tr>
                <th>Player</th>
                <th>Model</th>
                <th>W / L / D</th>
                <th>Score</th>
                <th>Memo v</th>
              </tr>
            </thead>
            <tbody>
              {players.map((p) => (
                <tr key={p.agentId}>
                  <td>{p.agentName || p.agentId}</td>
                  <td className="muted">{p.model}</td>
                  <td>
                    {p.wins} / {p.losses} / {p.draws}
                  </td>
                  <td>{p.totalScore.toFixed(1)}</td>
                  <td>{p.memoVersion}</td>
                </tr>
              ))}
              {players.length === 0 && (
                <tr>
                  <td colSpan={5} className="muted">
                    No players yet — check providers.toml
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </section>

        <section className="card">
          <h2>Recent Matches</h2>
          <table>
            <thead>
              <tr>
                <th>Game</th>
                <th>Players</th>
                <th>Status</th>
                <th>Result</th>
              </tr>
            </thead>
            <tbody>
              {matches.map((m) => (
                <tr key={m.id}>
                  <td>
                    <Link to={`/match/${m.id}`}>{m.gameTitle || "(untitled)"}</Link>
                  </td>
                  <td className="muted">
                    {m.agentAId} vs {m.agentBId}
                  </td>
                  <td>
                    <span className={`status status-${m.status}`}>
                      {STATUS_LABELS[m.status] ?? m.status}
                    </span>
                  </td>
                  <td>
                    {m.result
                      ? `${m.result.totalScoreA.toFixed(1)} – ${m.result.totalScoreB.toFixed(1)}` +
                        (m.result.winnerAgentId ? ` (${m.result.winnerAgentId})` : " (draw)")
                      : "—"}
                  </td>
                </tr>
              ))}
              {matches.length === 0 && (
                <tr>
                  <td colSpan={4} className="muted">
                    No matches yet
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </section>
      </div>
    </div>
  );
}
