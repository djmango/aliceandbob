import { createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";
import { ArenaService } from "./gen/aliceandbob/v1/service_pb";

// Dev: talk to the local server. Production: UI is served by the Rust
// server itself, so use the same origin. VITE_API_URL overrides both.
const baseUrl =
  import.meta.env.VITE_API_URL ??
  (import.meta.env.DEV ? "http://localhost:3030" : window.location.origin);

const transport = createConnectTransport({ baseUrl });

export const arena = createClient(ArenaService, transport);
