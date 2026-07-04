import { useEffect, useRef, useState } from "react";
import { useParams } from "react-router-dom";
import { arena } from "../api";
import type { GameSpec, MatchEvent, RoundResult } from "../gen/aliceandbob/v1/game_pb";
import { Adjudicator, MatchStatus, TurnStructure } from "../gen/aliceandbob/v1/game_pb";
import type { GetMatchResponse } from "../gen/aliceandbob/v1/service_pb";

interface ActionView {
  agentId: string;
  round: number;
  actionJson: string;
  reasoning: string;
}

type StreamState = "connecting" | "live" | "reconnecting" | "ended" | "error";

function sleep(ms: number, signal: AbortSignal) {
  return new Promise<void>((resolve, reject) => {
    const timer = setTimeout(resolve, ms);
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(timer);
        reject(new DOMException("Aborted", "AbortError"));
      },
      { once: true },
    );
  });
}

function isTerminalStatus(status: MatchStatus | undefined) {
  return status === MatchStatus.COMPLETED || status === MatchStatus.FAILED;
}

function formatFinalResult(totalA: number, totalB: number, winnerId: string) {
  if (winnerId) {
    return `Winner: ${winnerId} (${totalA.toFixed(1)} – ${totalB.toFixed(1)})`;
  }
  return `Draw (${totalA.toFixed(1)} – ${totalB.toFixed(1)})`;
}

export default function MatchViewer() {
  const { matchId } = useParams<{ matchId: string }>();
  const [spec, setSpec] = useState<GameSpec>();
  const [rounds, setRounds] = useState<RoundResult[]>([]);
  const [pendingActions, setPendingActions] = useState<ActionView[]>([]);
  const [currentRound, setCurrentRound] = useState(0);
  const [memoNotes, setMemoNotes] = useState<string[]>([]);
  const [finalResult, setFinalResult] = useState<string>("");
  const [matchError, setMatchError] = useState<string>("");
  const [streamState, setStreamState] = useState<StreamState>("connecting");
  const logEnd = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!matchId) return;
    const abort = new AbortController();
    let reconnectAttempt = 0;

    const resetLiveState = () => {
      setSpec(undefined);
      setRounds([]);
      setPendingActions([]);
      setCurrentRound(0);
      setMemoNotes([]);
      setFinalResult("");
      setMatchError("");
    };

    const hydrateFromGetMatch = (snap: GetMatchResponse) => {
      if (snap.spec) setSpec(snap.spec);
      if (snap.rounds.length > 0) {
        setRounds(snap.rounds.slice().sort((a, b) => a.round - b.round));
      }
      const summary = snap.summary;
      const result = summary?.result;
      if (summary?.status === MatchStatus.COMPLETED && result) {
        setFinalResult(
          formatFinalResult(result.totalScoreA, result.totalScoreB, result.winnerAgentId),
        );
      }
      if (summary?.status === MatchStatus.FAILED) {
        setMatchError("Match interrupted (server restarted while this match was running).");
      }
    };

    const handleEvent = (event: MatchEvent) => {
      const e = event.event;
      switch (e.case) {
        case "gameGenerated":
          setSpec(e.value.spec);
          break;
        case "roundStarted":
          setCurrentRound(e.value.round);
          setPendingActions([]);
          break;
        case "actionSubmitted": {
          const a = e.value.action;
          if (a) {
            setPendingActions((prev) => [
              ...prev,
              {
                agentId: a.agentId,
                round: a.round,
                actionJson: a.actionJson,
                reasoning: a.privateReasoning,
              },
            ]);
          }
          break;
        }
        case "roundScored":
          if (e.value.result) {
            const result = e.value.result;
            setRounds((prev) => [...prev.filter((r) => r.round !== result.round), result]);
            setPendingActions([]);
          }
          break;
        case "memoUpdated":
          setMemoNotes((prev) => [
            ...prev,
            `${e.value.agentId} updated its strategy memo to v${e.value.memoVersion}`,
          ]);
          break;
        case "matchCompleted": {
          const r = e.value.result;
          if (r) {
            setFinalResult(
              r.winnerAgentId
                ? `Winner: ${r.winnerAgentId} (${r.totalScoreA.toFixed(1)} – ${r.totalScoreB.toFixed(1)})`
                : `Draw (${r.totalScoreA.toFixed(1)} – ${r.totalScoreB.toFixed(1)})`,
            );
          }
          break;
        }
        case "matchError":
          setMatchError(e.value.message);
          break;
      }
    };

    (async () => {
      while (!abort.signal.aborted) {
        try {
          resetLiveState();
          setStreamState(reconnectAttempt > 0 ? "reconnecting" : "connecting");

          for await (const event of arena.watchMatch({ matchId }, { signal: abort.signal })) {
            setStreamState("live");
            setMatchError("");
            handleEvent(event);
          }

          const snap = await arena.getMatch({ matchId }, { signal: abort.signal });
          hydrateFromGetMatch(snap);
          if (isTerminalStatus(snap.summary?.status)) {
            setStreamState("ended");
            return;
          }

          reconnectAttempt++;
          setStreamState("reconnecting");
          await sleep(Math.min(1000 * reconnectAttempt, 5000), abort.signal);
        } catch (e) {
          if (abort.signal.aborted) return;

          try {
            const snap = await arena.getMatch({ matchId }, { signal: abort.signal });
            hydrateFromGetMatch(snap);
            if (isTerminalStatus(snap.summary?.status)) {
              setStreamState("ended");
              return;
            }
          } catch {
            // keep retrying
          }

          reconnectAttempt++;
          setStreamState("reconnecting");
          setMatchError(
            reconnectAttempt === 1
              ? "Connection lost - retrying. This can happen during deploys or long pauses between LLM calls."
              : `Connection lost - retrying (${reconnectAttempt})…`,
          );
          try {
            await sleep(Math.min(1000 * reconnectAttempt, 5000), abort.signal);
          } catch {
            return;
          }
        }
      }
    })();

    return () => abort.abort();
  }, [matchId]);

  useEffect(() => {
    logEnd.current?.scrollIntoView({ behavior: "smooth" });
  }, [rounds, pendingActions]);

  return (
    <div className="page">
      <div className="match-header">
        <h2>{spec?.title ?? "Waiting for the Game Master..."}</h2>
        <span className={`status-pill stream-${streamState}`}>{streamState}</span>
      </div>

      {spec && (
        <section className="card">
          <div className="spec-meta">
            <span>
              {spec.turnStructure === TurnStructure.ALTERNATING ? "Alternating" : "Simultaneous"}{" "}
              turns
            </span>
            <span>{spec.numRounds} rounds</span>
          </div>
          <pre className="rules">{spec.rulesText}</pre>
          <details>
            <summary>Payoffs</summary>
            <p>{spec.payoffDescription}</p>
            {spec.payoffMatrix.length > 0 && (
              <table className="payoff-table">
                <thead>
                  <tr>
                    <th>A plays</th>
                    <th>B plays</th>
                    <th>A gets</th>
                    <th>B gets</th>
                  </tr>
                </thead>
                <tbody>
                  {spec.payoffMatrix.map((p, i) => (
                    <tr key={i}>
                      <td>{p.actionA}</td>
                      <td>{p.actionB}</td>
                      <td>{p.scoreA}</td>
                      <td>{p.scoreB}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </details>
        </section>
      )}

      <section className="rounds">
        {rounds
          .slice()
          .sort((a, b) => a.round - b.round)
          .map((r) => (
            <div key={r.round} className="card round-card">
              <div className="round-title">
                <strong>Round {r.round}</strong>
                <span className="muted">
                  {r.adjudicatedBy === Adjudicator.GM ? "GM adjudicated" : "engine scored"} ·{" "}
                  {r.scoreA >= 0 ? "+" : ""}
                  {r.scoreA} / {r.scoreB >= 0 ? "+" : ""}
                  {r.scoreB}
                </span>
              </div>
              <div className="actions-grid">
                {r.actions.map((a) => (
                  <div key={a.agentId} className="action-box">
                    <div className="action-agent">{a.agentId}</div>
                    <code>{a.actionJson}</code>
                    {a.privateReasoning && (
                      <details>
                        <summary>private reasoning</summary>
                        <p>{a.privateReasoning}</p>
                      </details>
                    )}
                  </div>
                ))}
              </div>
              {r.narration && <p className="narration">{r.narration}</p>}
            </div>
          ))}

        {pendingActions.length > 0 && (
          <div className="card round-card pending">
            <div className="round-title">
              <strong>Round {currentRound}</strong>
              <span className="muted">in progress…</span>
            </div>
            <div className="actions-grid">
              {pendingActions.map((a, i) => (
                <div key={i} className="action-box">
                  <div className="action-agent">{a.agentId}</div>
                  <code>{a.actionJson}</code>
                </div>
              ))}
            </div>
          </div>
        )}
        <div ref={logEnd} />
      </section>

      {memoNotes.map((note, i) => (
        <div key={i} className="memo-note">
          {note}
        </div>
      ))}
      {finalResult && <div className="final-result">{finalResult}</div>}
      {matchError && <div className="error-banner">{matchError}</div>}
    </div>
  );
}
