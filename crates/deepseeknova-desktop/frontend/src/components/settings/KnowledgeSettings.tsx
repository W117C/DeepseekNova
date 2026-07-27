/**
 * KnowledgeSettings.tsx — 知识库：Wiki / 知识卡片 / 检索参数（get_wiki_pages · get_knowledge_cards）
 */

import { useEffect, useState } from "react";
import { getWikiPages, getKnowledgeCards } from "../../bridge";
import { SectionHeader, SettingRow } from "./Shared";

export default function KnowledgeSettings() {
  const [wikiCount, setWikiCount] = useState<number | null>(null);
  const [cardCount, setCardCount] = useState<number | null>(null);

  useEffect(() => {
    getWikiPages()
      .then((r: any) => setWikiCount(Array.isArray(r) ? r.length : r?.pages?.length ?? 0))
      .catch(() => setWikiCount(0));
    getKnowledgeCards()
      .then((r: any) => setCardCount(Array.isArray(r) ? r.length : r?.cards?.length ?? 0))
      .catch(() => setCardCount(0));
  }, []);

  return (
    <div>
      <SectionHeader title="知识库" desc="get_wiki_pages · get_knowledge_cards" />

      <SettingRow label="Repo Wiki" desc="架构 / API / 依赖图">
        <span className="tag">{wikiCount === null ? "…" : `${wikiCount} 页`}</span>
      </SettingRow>
      <SettingRow label="知识卡片" desc="置信度标注">
        <span className="tag">{cardCount === null ? "…" : `${cardCount} 张`}</span>
      </SettingRow>
      <SettingRow label="Embedding 模型" desc="检索向量化（即将支持配置）">
        <span className="tag">bge-m3 · 本地</span>
      </SettingRow>
      <SettingRow label="检索参数" desc="top_k · 相似度阈值 · rerank（即将支持配置）">
        <span className="tag">8 · 0.35 · 开</span>
      </SettingRow>
      <SettingRow label="重新生成" desc="从当前代码库刷新（即将支持）">
        <button className="btn" disabled title="即将支持">刷新</button>
      </SettingRow>
    </div>
  );
}
