import { useEffect, useState } from "react";
import { arena } from "../api";
import { agentLabel, isPlayer } from "../agents";
import type { AgentConfig, Memo } from "../gen/aliceandbob/v1/agents_pb";

export default function MemoLineage() {
  const [agents, setAgents] = useState<AgentConfig[]>([]);
  const [selected, setSelected] = useState("");
  const [memos, setMemos] = useState<Memo[]>([]);
  const [error, setError] = useState("");

  useEffect(() => {
    arena
      .listAgents({})
      .then((r) => {
        const players = r.agents.filter(isPlayer);
        setAgents(players);
        if (players.length > 0) setSelected(players[0].id);
      })
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    if (!selected) return;
    arena
      .getMemoLineage({ agentId: selected })
      .then((r) => setMemos(r.memos))
      .catch((e) => setError(String(e)));
  }, [selected]);

  return (
    <div className="page">
      <section className="card">
        <h2>Memo Lineage</h2>
        <p className="muted">
          A player's strategy memo is its evolving "genome" — rewritten after every match. Watch
          strategies develop across generations.
        </p>
        <label>
          Player{" "}
          <select value={selected} onChange={(e) => setSelected(e.target.value)}>
            {agents.map((a) => (
              <option key={a.id} value={a.id}>
                {agentLabel(a)}
              </option>
            ))}
          </select>
        </label>
      </section>

      {error && <div className="error-banner">{error}</div>}

      {memos
        .slice()
        .reverse()
        .map((m) => (
          <section key={m.id} className="card memo-card">
            <div className="round-title">
              <strong>v{m.version}</strong>
              <span className="muted">
                {m.matchId ? `after match ${m.matchId.slice(0, 8)}` : "seed"} ·{" "}
                {new Date(Number(m.createdAtMs)).toLocaleString()}
              </span>
            </div>
            <pre className="rules">{m.content}</pre>
          </section>
        ))}
      {memos.length === 0 && (
        <section className="card">
          <p className="muted">No memos yet — play a match first.</p>
        </section>
      )}
    </div>
  );
}
