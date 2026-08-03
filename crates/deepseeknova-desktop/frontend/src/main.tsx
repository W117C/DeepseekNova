import { render } from "solid-js/web"
import "@opencode-ai/ui/styles/index.css"

export default function App() {
  return <div data-smoke-test>DeepseekNova + opencode UI</div>
}

render(() => <App />, document.getElementById("root") as HTMLElement)