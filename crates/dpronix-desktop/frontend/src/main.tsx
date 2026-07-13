import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles.css";

// Error boundary — catches render errors without crashing the webview
class ErrorBoundary extends React.Component<
  { children: React.ReactNode },
  { error: Error | null }
> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  render() {
    if (this.state.error) {
      return (
        <div style={{ padding: 32, color: "#f85149" }}>
          <h2>Something went wrong</h2>
          <pre style={{ marginTop: 8, fontFamily: "monospace", fontSize: 13, whiteSpace: "pre-wrap" }}>
            {this.state.error.message}
          </pre>
          <button
            onClick={() => this.setState({ error: null })}
            style={{ marginTop: 12, padding: "8px 16px", cursor: "pointer" }}
          >
            Retry
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
);
