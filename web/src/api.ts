import { createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";
import { ArenaService } from "./gen/aliceandbob/v1/service_pb";

const transport = createConnectTransport({
  baseUrl: import.meta.env.VITE_API_URL ?? "http://localhost:3030",
});

export const arena = createClient(ArenaService, transport);
