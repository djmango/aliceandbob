import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { createBrowserRouter, RouterProvider } from "react-router-dom";
import App from "./App";
import Dashboard from "./pages/Dashboard";
import MatchViewer from "./pages/MatchViewer";
import MemoLineage from "./pages/MemoLineage";
import "./index.css";

const router = createBrowserRouter([
  {
    path: "/",
    element: <App />,
    children: [
      { index: true, element: <Dashboard /> },
      { path: "match/:matchId", element: <MatchViewer /> },
      { path: "memos", element: <MemoLineage /> },
    ],
  },
]);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <RouterProvider router={router} />
  </StrictMode>,
);
