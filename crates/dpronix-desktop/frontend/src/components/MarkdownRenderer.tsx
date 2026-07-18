/**
 * MarkdownRenderer — renders assistant text with syntax-highlighted code blocks.
 */
import { useMemo } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

/** Inline code */
function InlineCode({ children }: { children?: React.ReactNode }) {
  return <code className="md-inline-code">{children}</code>;
}

/** Fenced code block with language label */
function CodeBlock({ className, children }: { className?: string; children?: React.ReactNode }) {
  const lang = className?.replace("language-", "") ?? "";
  const code = String(children).replace(/\n$/, "");
  return (
    <div className="md-code-block">
      {lang && <div className="md-code-lang">{lang}</div>}
      <pre><code className={className}>{code}</code></pre>
    </div>
  );
}

interface MarkdownRendererProps {
  content: string;
}

export default function MarkdownRenderer({ content }: MarkdownRendererProps) {
  const components = useMemo(() => ({
    code({ className, children, ...props }: any) {
      const inline = !className;
      if (inline) return <InlineCode {...props}>{children}</InlineCode>;
      return <CodeBlock className={className}>{children}</CodeBlock>;
    },
    a({ href, children }: any) {
      return <a href={href} target="_blank" rel="noopener noreferrer">{children}</a>;
    },
  }), []);

  return (
    <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
      {content}
    </ReactMarkdown>
  );
}
