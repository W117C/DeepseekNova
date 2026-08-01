/**
 * MarkdownRenderer.tsx — Markdown 渲染 + 代码块复制
 */

import { useState, useCallback } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { Components } from "react-markdown";

export default function MarkdownRenderer({ content }: { content: string }) {
  const [copied, setCopied] = useState<string | null>(null);

  const handleCopy = useCallback((text: string, id: string) => {
    navigator.clipboard.writeText(text).then(() => {
      setCopied(id);
      setTimeout(() => setCopied(null), 2000);
    });
  }, []);

  const components: Components = {
    pre(props: React.ComponentProps<"pre">) {
      const { children, ...rest } = props;
      // 提取代码文本
      const codeEl = children as React.ReactElement<{
        className?: string;
        children?: React.ReactNode;
      }>;
      const codeText =
        typeof codeEl?.props?.children === "string"
          ? codeEl.props.children
          : Array.isArray(codeEl?.props?.children)
            ? (codeEl.props.children as React.ReactNode[]).join("")
            : "";

      const id = `code-${Math.random().toString(36).slice(2, 9)}`;

      return (
        <div className="code-block">
          <div className="code-block-header">
            <span>{codeEl?.props?.className?.replace("language-", "") || "code"}</span>
            <button className="copy-btn" onClick={() => handleCopy(codeText, id)}>
              {copied === id ? "✓ 已复制" : "复制"}
            </button>
          </div>
          <div className="code-block-content">
            <pre {...rest}>{children}</pre>
          </div>
        </div>
      );
    },
    code(props: React.ComponentProps<"code">) {
      const { children, ...rest } = props;
      return <code {...rest}>{children}</code>;
    },
    a(props: React.ComponentProps<"a">) {
      const { children, href, ...rest } = props;
      return (
        <a
          href={href}
          target="_blank"
          rel="noopener noreferrer"
          style={{ color: "var(--blue)" }}
          {...rest}
        >
          {children}
        </a>
      );
    },
    table(props: React.ComponentProps<"table">) {
      const { children, ...rest } = props;
      return (
        <div style={{ overflowX: "auto" }}>
          <table {...rest}>{children}</table>
        </div>
      );
    },
  };

  return (
    <ReactMarkdown components={components} remarkPlugins={[remarkGfm]}>
      {content}
    </ReactMarkdown>
  );
}
