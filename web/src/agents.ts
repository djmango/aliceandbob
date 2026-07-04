import type { AgentConfig } from "./gen/aliceandbob/v1/agents_pb";
import { AgentRole } from "./gen/aliceandbob/v1/agents_pb";

/** Connect JSON sometimes delivers enum names as strings before decode. */
export function isPlayer(agent: AgentConfig): boolean {
  return (
    agent.role === AgentRole.PLAYER ||
    agent.role === ("AGENT_ROLE_PLAYER" as unknown as AgentRole)
  );
}

export function isGameMaster(agent: AgentConfig): boolean {
  return (
    agent.role === AgentRole.GAME_MASTER ||
    agent.role === ("AGENT_ROLE_GAME_MASTER" as unknown as AgentRole)
  );
}

export function agentLabel(agent: AgentConfig): string {
  return `${agent.name} · ${agent.provider} · ${agent.model}`;
}
