/**
 * MessageItem.tsx — 用户/助手消息渲染（mockup 定稿：用户右 / AI 左 + 圆形渐变头像）
 */

import type { Message } from "../types";
import MarkdownRenderer from "./MarkdownRenderer";

export default function MessageItem({ message }: { message: Message }) {
  const isUser = message.role === "user";

  return (
    <div className={`msg ${isUser ? "user" : "assistant"}`}>
      <div className="msg-av">{isUser ? "U" : "N"}</div>
      <div className="msg-body">
        <div className="msg-who">{isUser ? "你" : "DeepseekNova"}</div>
        {isUser ? (
          <div className="msg-bubble">{message.content}</div>
        ) : (
          <div className="message-content" style={{ paddingLeft: 0 }}>
            <MarkdownRenderer content={message.content} />
          </div>
        )}
      </div>
    </div>
  );
}
