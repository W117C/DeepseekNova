import { render } from "solid-js/web"
import "./index.css"

export default function App() {
  return <div data-smoke-test>DeepseekNova + opencode UI</div>
}

render(() => <App />, document.getElementById("root") as HTMLElement)