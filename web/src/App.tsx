import { NavLink, Outlet } from "react-router-dom";

export default function App() {
  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <span className="brand-alice">Alice</span>
          <span className="brand-amp">&</span>
          <span className="brand-bob">Bob</span>
          <span className="brand-sub">LLM Game Theory Arena</span>
        </div>
        <nav>
          <NavLink to="/" end>
            Dashboard
          </NavLink>
          <NavLink to="/memos">Memo Lineage</NavLink>
        </nav>
      </header>
      <main>
        <Outlet />
      </main>
    </div>
  );
}
