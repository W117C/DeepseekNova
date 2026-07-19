import { memo, useState, useEffect, useRef } from "react";
import type { ProviderSummary } from "../types";

interface Props {
  providers: ProviderSummary[];
  currentModel: string;
  onSwitch: (provider: string, model: string) => void;
}

const ModelSelector = memo(({ providers, currentModel, onSwitch }: Props) => {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, []);

  const activeProvider = providers.find(p => p.connected) || providers[0];

  return (
    <div className="dp-model-selector" ref={ref}>
      <button className="dp-model-current" onClick={() => setOpen(!open)}>
        <span className={`dp-dot ${activeProvider?.connected ? "on" : "off"}`} />
        <span className="dp-model-name">{currentModel}</span>
        <span className="dp-model-caret">▼</span>
      </button>
      {open && (
        <div className="dp-model-dropdown">
          {providers.map(p => (
            <div
              key={p.name}
              className={`dp-model-option ${p.model === currentModel ? "selected" : ""}`}
              onClick={() => { onSwitch(p.name, p.model || "default"); setOpen(false); }}
            >
              <span className={`dp-dot ${p.connected ? "on" : "off"}`} />
              <div>
                <div className="dp-opt-name">{p.name}</div>
                <div className="dp-opt-model dp-muted">{p.model || "default"}</div>
              </div>
            </div>
          ))}
          {providers.length === 0 && <div className="dp-dropdown-empty">暂无可用 Provider</div>}
        </div>
      )}
    </div>
  );
});

export default ModelSelector;
