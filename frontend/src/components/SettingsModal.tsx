import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  addLibrary,
  cancelSyncTask,
  changeLibraryPath,
  deleteLibrary,
  getConfig,
  getSettings,
  recoverCovers,
  resetLlmUsage,
  saveLibrary,
  saveSettings,
  switchLibrary,
  webdavPull,
  webdavPush,
  webdavTest,
} from "../api/tauri";
import type {
  LibraryProfile,
  LlmProvider,
  LlmUsageRow,
  ModelPricing,
  TaskProgressEvent,
  WebDavCfg,
  WebDavReport,
} from "../types";
import { pickDirectory } from "../api/picker";
import { applyTheme, THEME_PRESETS } from "../theme";
import { useConfirm } from "./ConfirmDialog";

/** 书库卡片数据（新建未保存的为草稿） */
type LibDraft = LibraryProfile & { __draft?: boolean };

let providerSeq = 0;

type Tab = "library" | "llm" | "webdav";

/** 配置弹窗：左侧 书库 / LLM 选项卡；各组内多个条目以并列子选项卡切换 */
export default function SettingsModal({ onClose }: { onClose: () => void }) {
  const { confirm, confirmElement } = useConfirm();
  const [tab, setTab] = useState<Tab>("library");
  const [providers, setProviders] = useState<LlmProvider[]>([]);
  const [defaultProvider, setDefaultProvider] = useState<string | null>(null);
  const [defaultModel, setDefaultModel] = useState<string | null>(null);
  const [retryCount, setRetryCount] = useState<number | null>(2);
  const [budgetRmb, setBudgetRmb] = useState<number | null>(null);
  const [usage, setUsage] = useState<LlmUsageRow[]>([]);
  const [usageTotalTokens, setUsageTotalTokens] = useState(0);
  const [usageTotalCost, setUsageTotalCost] = useState<number | null>(null);
  const [usageUnpriced, setUsageUnpriced] = useState(0);
  /**
   * 价格编辑表（model → 输入/输出价）。
   * 显式配置（pricing）优先展示；未配置的行展示后端解析的生效价
   * （内置预设）——用户首次编辑时以生效价为底写入显式配置，
   * 避免只改一项把另一项清零。
   */
  const [pricing, setPricing] = useState<ModelPricing[]>([]);
  const [pricingEffective, setPricingEffective] = useState<ModelPricing[]>([]);
  const [libraries, setLibraries] = useState<LibDraft[]>([]);
  const [drafts, setDrafts] = useState<Record<string, LibDraft>>({});
  const [currentLib, setCurrentLib] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [libError, setLibError] = useState<string | null>(null);
  const [resettingUsage, setResettingUsage] = useState(false);
  /** 手动封面恢复（busy / 结果提示） */
  const [coverRecovering, setCoverRecovering] = useState(false);
  const [coverResult, setCoverResult] = useState<string | null>(null);

  /** 手动触发封面缺失恢复（与启动自动恢复共用后端逻辑） */
  const onRecoverCovers = async () => {
    if (coverRecovering) return;
    setCoverRecovering(true);
    setCoverResult(null);
    setLibError(null);
    try {
      const r = await recoverCovers();
      setCoverResult(
        r.missing === 0
          ? `扫描 ${r.scanned} 本书，封面全部正常`
          : `缺失 ${r.missing}，恢复 ${r.recovered}` +
              (r.failed > 0 ? `，失败 ${r.failed}（详见日志）` : ""),
      );
      // 有恢复时刷新书架与详情（封面已回填）
      if (r.recovered > 0) window.dispatchEvent(new CustomEvent("mna:library-refresh"));
    } catch (e) {
      setLibError(String(e));
    } finally {
      setCoverRecovering(false);
    }
  };

  // 子选项卡选中项
  const [libSelId, setLibSelId] = useState<string | null>(null);
  const [provSel, setProvSel] = useState(0);
  // WebDav 页签选中的书库
  const [webdavSelId, setWebdavSelId] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    Promise.all([getSettings(), getConfig()])
      .then(([s, cfg]) => {
        if (cancelled) return;
        setProviders(s.llm.providers);
        setDefaultProvider(s.llm.default_provider);
        setDefaultModel(s.llm.default_model);
        setRetryCount(s.llm.retry_count ?? 2);
        setBudgetRmb(s.llm.budget_rmb);
        setUsage(s.usage);
        setUsageTotalTokens(s.usage_total_tokens);
        setUsageTotalCost(s.usage_total_cost);
        setUsageUnpriced(s.usage_unpriced);
        setPricing(s.llm.pricing);
        setPricingEffective(s.pricing_effective);
        setLibraries(cfg.libraries);
        setCurrentLib(cfg.current_library);
        setLibSelId(cfg.current_library ?? cfg.libraries[0]?.id ?? null);
        const cur = cfg.libraries.find((l) => l.id === cfg.current_library);
        applyTheme(cur?.theme ?? null);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
    return () => {
      cancelled = true;
    };
  }, []);

  /** 重新拉取用量统计（清零后刷新展示） */
  const reloadUsage = async () => {
    try {
      const s = await getSettings();
      setUsage(s.usage);
      setUsageTotalTokens(s.usage_total_tokens);
      setUsageTotalCost(s.usage_total_cost);
        setUsageUnpriced(s.usage_unpriced);
    } catch (e) {
      setError(String(e));
    }
  };

  const updateProvider = (idx: number, patch: Partial<LlmProvider>) => {
    setProviders((prev) =>
      prev.map((p, i) => (i === idx ? { ...p, ...patch } : p)),
    );
  };

  const addProvider = () => {
    providerSeq += 1;
    const name = `服务商 ${providerSeq}`;
    setProviders((prev) => {
      const next = [...prev, { name, base_url: "", api_key: "", models: [] }];
      setProvSel(next.length - 1);
      return next;
    });
    setDefaultProvider((d) => d ?? name);
  };

  const removeProvider = (idx: number) => {
    const removed = providers[idx];
    setProviders((prev) => prev.filter((_, i) => i !== idx));
    setProvSel((i) => Math.max(0, Math.min(i, providers.length - 2)));
    if (defaultProvider === removed.name) {
      setDefaultProvider(null);
      setDefaultModel(null);
    }
  };

  const addModel = (idx: number, model: string) => {
    const m = model.trim();
    if (!m) return;
    setProviders((prev) =>
      prev.map((p, i) =>
        i === idx && !p.models.includes(m)
          ? { ...p, models: [...p.models, m] }
          : p,
      ),
    );
  };

  const removeModel = (idx: number, model: string) => {
    setProviders((prev) =>
      prev.map((p, i) =>
        i === idx ? { ...p, models: p.models.filter((m) => m !== model) } : p,
      ),
    );
    if (defaultProvider === providers[idx].name && defaultModel === model) {
      setDefaultModel(null);
    }
  };

  /** 点击模型 chip 设为全局默认（provider + model 成对生效） */
  const setDefault = (p: LlmProvider, model: string) => {
    setDefaultProvider(p.name);
    setDefaultModel(model);
  };

  const onSaveLlm = async () => {
    setSaving(true);
    setError(null);
    setSaved(false);
    try {
      await saveSettings({
        providers,
        default_provider: defaultProvider,
        default_model: defaultModel,
        retry_count: retryCount,
        budget_rmb: budgetRmb,
        pricing,
      });
      setSaved(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  /** 清零用量统计（确认后执行；不影响书籍数据） */
  const onResetUsage = async () => {
    if (resettingUsage) return;
    const ok = await confirm({
      title: "清零用量统计",
      message:
        "确定清零全部 token 用量与预算进度吗？\n\n书籍数据不受影响；适合按月/按周期重置预算。",
      okLabel: "清零",
      danger: true,
    });
    if (!ok) return;
    setResettingUsage(true);
    try {
      await resetLlmUsage();
      await reloadUsage();
    } catch (e) {
      setError(String(e));
    } finally {
      setResettingUsage(false);
    }
  };

  // ---- 价格编辑表 ----

  /** 编辑行集合：生效价格模型 ∪ 已显式配置模型（去重，保持插入序） */
  const pricingRows: string[] = (() => {
    const seen = new Set<string>();
    const rows: string[] = [];
    const push = (m: string) => {
      const key = m.trim();
      if (!key || seen.has(key)) return;
      seen.add(key);
      rows.push(key);
    };
    pricingEffective.forEach((p) => push(p.model));
    pricing.forEach((p) => push(p.model));
    providers.forEach((p) => p.models.forEach(push));
    return rows;
  })();

  /** 某模型的展示值：显式配置 > 生效价（内置预设）> 空 */
  const pricingValueOf = (model: string): ModelPricing =>
    pricing.find((p) => p.model === model) ??
    pricingEffective.find((p) => p.model === model) ?? {
      model,
      input_per_m: 0,
      output_per_m: 0,
    };

  /** 编辑某模型价格：以当前展示值为底合并改动，写入显式配置 */
  const patchPricing = (model: string, patch: Partial<ModelPricing>) =>
    setPricing((prev) => {
      const base = pricingValueOf(model);
      const merged = { ...base, ...patch, model };
      const exists = prev.some((p) => p.model === model);
      return exists
        ? prev.map((p) => (p.model === model ? merged : p))
        : [...prev, merged];
    });

  /** 用量行成本的展示格式：大额两位小数，小额四位小数 */
  const fmtCost = (c: number | null | undefined) =>
    c == null ? "—" : `¥${c >= 1 ? c.toFixed(2) : c.toFixed(4)}`;

  // ---- 书库子选项卡 ----

  const draftOf = (lib: LibDraft): LibDraft => drafts[lib.id] ?? lib;
  const patchDraft = (lib: LibDraft, p: Partial<LibDraft>) =>
    setDrafts((prev) => ({ ...prev, [lib.id]: { ...draftOf(lib), ...p } }));
  const clearDraft = (id: string) =>
    setDrafts((prev) => {
      const next = { ...prev };
      delete next[id];
      return next;
    });

  const addLibraryDraft = () => {
    const id = `draft-${Date.now()}`;
    setLibraries((prev) => [
      ...prev,
      {
        id,
        name: "",
        title: "新书库",
        path: "",
        // 新书库默认取"暗金"成套配色（应用默认主题）
        theme: { ...THEME_PRESETS[0].colors },
        default_tags: ["推理小说"],
        webdav: { enabled: false, url: "", username: "", password: "", remote_dir: "" },
        __draft: true,
      },
    ]);
    setLibSelId(id);
  };

  const selectedLib = (() => {
    if (libSelId) {
      const found = libraries.find((l) => l.id === libSelId);
      if (found) return found;
    }
    return libraries[0] ?? null;
  })();

  // WebDav 页签：仅展示已保存书库；默认当前书库
  const selectedWebdavLib = (() => {
    const saved = libraries.filter((l) => !l.__draft);
    if (webdavSelId) {
      const found = saved.find((l) => l.id === webdavSelId);
      if (found) return found;
    }
    return saved.find((l) => l.id === currentLib) ?? saved[0] ?? null;
  })();

  /** 直接修补某书库的 WebDav 配置（独立于书库草稿） */
  const patchWebdav = (id: string, p: Partial<WebDavCfg>) =>
    setLibraries((prev) =>
      prev.map((l) => {
        if (l.id !== id) return l;
        // 兼容旧配置缺 webdav 段的情况
        const base: WebDavCfg = l.webdav ?? {
          enabled: false,
          url: "",
          username: "",
          password: "",
          remote_dir: "",
        };
        return { ...l, webdav: { ...base, ...p } };
      }),
    );

  const onLibChanged = (list: LibDraft[], libId?: string, newCurrent?: string) => {
    setLibraries(list);
    if (libId) clearDraft(libId);
    if (!list.some((l) => l.id === libSelId)) {
      setLibSelId(list[0]?.id ?? null);
    }
    if (newCurrent !== undefined) {
      setCurrentLib(newCurrent);
      applyTheme(list.find((l) => l.id === newCurrent)?.theme ?? null);
    }
  };

  const provIdx = Math.min(provSel, Math.max(0, providers.length - 1));
  const provider = providers[provIdx] ?? null;

  return (
    <div className="modal-overlay">
      <div className="modal settings-modal">
        <div className="settings-tabs">
          <button
            className={`settings-tab ${tab === "library" ? "active" : ""}`}
            onClick={() => setTab("library")}
          >
            书 库
          </button>
          <button
            className={`settings-tab ${tab === "llm" ? "active" : ""}`}
            onClick={() => setTab("llm")}
          >
            LLM
          </button>
          <button
            className={`settings-tab ${tab === "webdav" ? "active" : ""}`}
            onClick={() => setTab("webdav")}
          >
            WebDav
          </button>
          <span className="bulk-flex" />
          <button className="chat-close" onClick={onClose} title="关闭">
            ×
          </button>
        </div>

        <div className="settings-body">
          {loading ? (
            <div className="state">加载中…</div>
          ) : tab === "library" ? (
            <>
              {/* 书库并列子选项卡 */}
              <div className="subtabs">
                {libraries.map((l) => (
                  <button
                    key={l.id}
                    className={`subtab ${selectedLib?.id === l.id ? "active" : ""}`}
                    onClick={() => setLibSelId(l.id)}
                  >
                    {draftOf(l).title || "未命名"}
                    {currentLib === l.id && <span className="subtab-dot">●</span>}
                    {l.__draft && <span className="subtab-draft">新</span>}
                  </button>
                ))}
                <button className="subtab add" onClick={addLibraryDraft}>
                  ＋ 新建
                </button>
              </div>

              {libError && <div className="modal-error">{libError}</div>}

              {/* 封面缺失自检与恢复（启动时自动执行；此处可手动重跑） */}
              <div className="lib-tools-row">
                <button
                  className="btn small"
                  onClick={() => void onRecoverCovers()}
                  disabled={coverRecovering || loading}
                  title="扫描全部书籍，对缺失封面按 本地缓存 → EPUB 内嵌 → 远程来源 依次恢复"
                >
                  {coverRecovering ? "恢复中…" : "恢复缺失封面"}
                </button>
                {coverResult && <span className="webdav-status ok">{coverResult}</span>}
              </div>

              {selectedLib && (
                <LibraryCard
                  key={selectedLib.id}
                  draft={draftOf(selectedLib)}
                  isDraft={!!selectedLib.__draft}
                  current={currentLib === selectedLib.id}
                  canDelete={libraries.length > 1}
                  onPatch={(p) => patchDraft(selectedLib, p)}
                  onError={setLibError}
                  onSaved={(list, id, newCurrent) => onLibChanged(list, id, newCurrent)}
                  onSwitched={(newCurrent) => onLibChanged(libraries, undefined, newCurrent)}
                  onDeleted={(list) => onLibChanged(list, selectedLib.id)}
                />
              )}
            </>
          ) : tab === "webdav" ? (
            <>
              {/* WebDav 子选项卡：每个已保存书库一页 */}
              <div className="subtabs">
                {libraries.filter((l) => !l.__draft).map((l) => (
                  <button
                    key={l.id}
                    className={`subtab ${selectedWebdavLib?.id === l.id ? "active" : ""}`}
                    onClick={() => setWebdavSelId(l.id)}
                  >
                    {l.title || "未命名"}
                    {l.webdav?.enabled && <span className="subtab-dot">●</span>}
                  </button>
                ))}
              </div>

              {libError && <div className="modal-error">{libError}</div>}

              {selectedWebdavLib ? (
                <WebDavPanel
                  key={selectedWebdavLib.id}
                  lib={selectedWebdavLib}
                  isCurrent={selectedWebdavLib.id === currentLib}
                  onPatch={(p) => patchWebdav(selectedWebdavLib.id, p)}
                  onError={setLibError}
                  onSaved={(list) => {
                    setLibraries(list);
                    clearDraft(selectedWebdavLib.id);
                  }}
                />
              ) : (
                <div className="form-hint">
                  尚无已保存的书库，请先在「书库」页签创建
                </div>
              )}
            </>
          ) : (
            <>
              {/* LLM 服务商并列子选项卡 */}
              <div className="subtabs">
                {providers.map((p, i) => (
                  <button
                    key={i}
                    className={`subtab ${i === provIdx ? "active" : ""}`}
                    onClick={() => setProvSel(i)}
                  >
                    {p.name || `服务商 ${i + 1}`}
                    {defaultProvider === p.name && <span className="subtab-dot">●</span>}
                  </button>
                ))}
                <button className="subtab add" onClick={addProvider}>
                  ＋ 添加
                </button>
              </div>

              {error && <div className="modal-error">{error}</div>}

              {provider ? (
                <div className="provider-card">
                  <div className="provider-head">
                    <input
                      className="provider-name"
                      value={provider.name}
                      placeholder="服务商名称"
                      onChange={(e) => updateProvider(provIdx, { name: e.target.value })}
                    />
                    <button
                      className="btn small danger-btn"
                      onClick={() => removeProvider(provIdx)}
                      disabled={providers.length === 0}
                    >
                      移除
                    </button>
                  </div>

                  <label className="modal-label">
                    API Endpoint
                    <input
                      value={provider.base_url}
                      placeholder="https://api.openai.com/v1"
                      onChange={(e) => updateProvider(provIdx, { base_url: e.target.value })}
                    />
                  </label>

                  <label className="modal-label">
                    API Key
                    <input
                      type="password"
                      value={provider.api_key}
                      placeholder="sk-…"
                      onChange={(e) => updateProvider(provIdx, { api_key: e.target.value })}
                    />
                  </label>

                  <div className="modal-label">模型（点选设为默认）</div>
                  <div className="chip-row">
                    {provider.models.map((m) => {
                      const isDefault =
                        defaultProvider === provider.name && defaultModel === m;
                      return (
                        <span
                          key={m}
                          className={`chip ${isDefault ? "default" : ""}`}
                          onClick={() => setDefault(provider, m)}
                        >
                          {isDefault && <span className="chip-dot">●</span>}
                          {m}
                          <span
                            className="chip-x"
                            onClick={(e) => {
                              e.stopPropagation();
                              removeModel(provIdx, m);
                            }}
                          >
                            ×
                          </span>
                        </span>
                      );
                    })}
                    {provider.models.length === 0 && (
                      <span className="form-hint">尚无模型</span>
                    )}
                  </div>
                  <ModelInput onAdd={(m) => addModel(provIdx, m)} />
                </div>
              ) : (
                <div className="form-hint warn-hint">
                  尚未添加服务商，简介融合 / AI 书评将不可用
                </div>
              )}

              <label className="modal-label">
                调用失败重试次数（0 = 不重试）
                <input
                  type="number"
                  min={0}
                  max={10}
                  value={retryCount ?? 0}
                  onChange={(e) => {
                    const n = Number(e.target.value);
                    setRetryCount(
                      e.target.value === "" || Number.isNaN(n)
                        ? null
                        : Math.max(0, Math.min(10, Math.floor(n))),
                    );
                  }}
                  disabled={saving}
                  style={{ maxWidth: 120 }}
                />
              </label>

              <label className="modal-label">
                花费预算（元，累计成本达到后自动中断 LLM 调用；留空 = 不限）
                <input
                  type="number"
                  min={0}
                  step={0.5}
                  value={budgetRmb ?? ""}
                  placeholder="如 20"
                  onChange={(e) => {
                    const n = Number(e.target.value);
                    setBudgetRmb(
                      e.target.value === "" || Number.isNaN(n) || n <= 0
                        ? null
                        : n,
                    );
                  }}
                  disabled={saving}
                  style={{ maxWidth: 200 }}
                />
              </label>

              <div className="modal-actions">
                {saved && <span className="save-ok">已保存</span>}
                {error && <span className="modal-error">{error}</span>}
                <button className="btn primary" onClick={onSaveLlm} disabled={saving}>
                  {saving ? "保存中…" : "保 存"}
                </button>
              </div>

              <div className="usage-block">
                <h2>Token 用量与成本</h2>

                {/* 预算进度（按累计成本换算） */}
                <div className="budget-bar">
                  <div className="budget-text">
                    累计 {usageTotalTokens.toLocaleString()} tokens
                    {usageTotalCost != null && ` · 预估成本 ${fmtCost(usageTotalCost)}`}
                    {budgetRmb
                      ? ` · 预算 ¥${budgetRmb}（${
                          usageTotalCost != null
                            ? Math.min(100, Math.round((usageTotalCost / budgetRmb) * 100))
                            : 0
                        }%）`
                      : " · 未设置预算"}
                    {budgetRmb != null &&
                      usageTotalCost != null &&
                      usageTotalCost >= budgetRmb && (
                        <span className="budget-exceeded"> · 已达预算，LLM 调用已中断</span>
                      )}
                  </div>
                  {budgetRmb != null && budgetRmb > 0 && (
                    <div className="progress-bar batch">
                      <div
                        className={`progress-fill ${
                          usageTotalCost != null && usageTotalCost >= budgetRmb ? "over" : ""
                        }`}
                        style={{
                          width: `${
                            usageTotalCost != null
                              ? Math.min(100, Math.round((usageTotalCost / budgetRmb) * 100))
                              : 0
                          }%`,
                        }}
                      />
                    </div>
                  )}
                  <div className="budget-hint">
                    每次调用按响应中的用量精确累计，成本按下方价格表换算（元）。
                    {usageUnpriced > 0 &&
                      ` ${usageUnpriced} 个模型未定价，预算与成本不含其用量。`}
                    {budgetRmb
                      ? " 达到预算后所有 LLM 功能自动中断（融合/翻译降级、书虫报错），清零用量或调整预算后恢复。"
                      : ""}
                  </div>
                </div>

                {usage.length === 0 ? (
                  <div className="form-hint">
                    暂无用量记录（简介融合、AI 书评、书虫调用后自动累计）
                  </div>
                ) : (
                  <table className="usage-table">
                    <thead>
                      <tr>
                        <th>服务商 / 模型</th>
                        <th>调用</th>
                        <th>输入</th>
                        <th>输出</th>
                        <th>总计</th>
                        <th>预估成本</th>
                        <th>最近使用</th>
                      </tr>
                    </thead>
                    <tbody>
                      {usage.map((u) => (
                        <tr key={u.model}>
                          <td>{u.model}</td>
                          <td>{u.calls}</td>
                          <td>{u.prompt_tokens.toLocaleString()}</td>
                          <td>{u.completion_tokens.toLocaleString()}</td>
                          <td className="total">{u.total_tokens.toLocaleString()}</td>
                          <td>{fmtCost(u.cost)}</td>
                          <td>{u.last_used ?? "—"}</td>
                        </tr>
                      ))}
                      <tr className="usage-total-row">
                        <td>合计</td>
                        <td>—</td>
                        <td>—</td>
                        <td>—</td>
                        <td className="total">{usageTotalTokens.toLocaleString()}</td>
                        <td>{fmtCost(usageTotalCost)}</td>
                        <td>—</td>
                      </tr>
                    </tbody>
                  </table>
                )}

                <div className="usage-actions">
                  <button
                    className="btn small"
                    onClick={() => void onResetUsage()}
                    disabled={resettingUsage || usage.length === 0}
                  >
                    {resettingUsage ? "清零中…" : "清零用量"}
                  </button>
                </div>
              </div>

              <div className="usage-block">
                <h2>模型价格（元 / 百万 tokens）</h2>
                <div className="form-hint">
                  成本 = 输入量 × 输入价 + 输出量 × 输出价。未配置的模型按内置参考价估算，
                  可直接修改覆盖；价格留空（0）的行保存时丢弃。
                </div>
                {pricingRows.length === 0 ? (
                  <div className="form-hint">添加服务商与模型后可在此配置价格</div>
                ) : (
                  <table className="usage-table pricing-table">
                    <thead>
                      <tr>
                        <th>模型</th>
                        <th>输入价</th>
                        <th>输出价</th>
                      </tr>
                    </thead>
                    <tbody>
                      {pricingRows.map((model) => {
                        const row = pricingValueOf(model);
                        return (
                          <tr key={model}>
                            <td>{model}</td>
                            <td>
                              <input
                                type="number"
                                min={0}
                                step={0.1}
                                value={row.input_per_m || ""}
                                placeholder="不计成本"
                                onChange={(e) =>
                                  patchPricing(model, {
                                    input_per_m:
                                      e.target.value === "" ? 0 : Number(e.target.value),
                                  })
                                }
                                disabled={saving}
                              />
                            </td>
                            <td>
                              <input
                                type="number"
                                min={0}
                                step={0.1}
                                value={row.output_per_m || ""}
                                placeholder="不计成本"
                                onChange={(e) =>
                                  patchPricing(model, {
                                    output_per_m:
                                      e.target.value === "" ? 0 : Number(e.target.value),
                                  })
                                }
                                disabled={saving}
                              />
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                )}
                <div className="modal-actions">
                  <button className="btn small primary" onClick={onSaveLlm} disabled={saving}>
                    {saving ? "保存中…" : "保存价格与预算"}
                  </button>
                </div>
              </div>
            </>
          )}
        </div>
      </div>
      {confirmElement}
    </div>
  );
}

/** WebDav 同步页签面板：启用开关 + 服务器地址 + 凭据 + 测试连接 / 保存 / 同步到云端 / 从云端同步 */
function WebDavPanel({
  lib,
  isCurrent,
  onPatch,
  onError,
  onSaved,
}: {
  lib: LibraryProfile;
  /** 是否当前书库（同步仅对当前书库生效） */
  isCurrent: boolean;
  onPatch: (p: Partial<WebDavCfg>) => void;
  onError: (msg: string | null) => void;
  onSaved: (list: LibraryProfile[]) => void;
}) {
  const wd = lib.webdav;
  // 当前执行中的动作（互斥；各按钮独立提示，避免"测试连接"把"同步"也变忙碌）
  const [task, setTask] = useState<null | "test" | "save" | "push" | "pull">(null);
  const [testOk, setTestOk] = useState<boolean | null>(null);
  const [report, setReport] = useState<WebDavReport | null>(null);
  // 最近一次同步的方向（结果文案区分推送/拉取）
  const [lastDir, setLastDir] = useState<"push" | "pull" | null>(null);
  // 同步实时进度（task-progress 事件中 phase 以 webdav- 开头的负载）与任务标识
  const [syncPhase, setSyncPhase] = useState<TaskProgressEvent | null>(null);
  const taskIdRef = useRef<string>("");

  // 订阅同步进度事件
  useEffect(() => {
    const un = listen<TaskProgressEvent>("task-progress", (e) => {
      if (e.payload?.phase?.startsWith("webdav")) setSyncPhase(e.payload);
    });
    return () => {
      un.then((fn) => fn()).catch(() => undefined);
    };
  }, []);

  const patch = (p: Partial<WebDavCfg>) => {
    setTestOk(null);
    setReport(null);
    onPatch(p);
  };

  /** 保存当前配置（同步前自动调用；不改动 task 状态） */
  const doSave = async (): Promise<boolean> => {
    onError(null);
    try {
      const list = await saveLibrary(lib);
      onSaved(list);
      return true;
    } catch (e) {
      onError(String(e));
      return false;
    }
  };

  const onTest = async () => {
    setTask("test");
    onError(null);
    setTestOk(null);
    try {
      await webdavTest(wd, lib.name);
      setTestOk(true);
    } catch (e) {
      setTestOk(false);
      onError(String(e));
    } finally {
      setTask(null);
    }
  };

  const onSave = async () => {
    setTask("save");
    await doSave();
    setTask(null);
  };

  /** 统一推送/拉取流程：先保存当前配置，保证远端动作与界面一致 */
  const onSync = async (dir: "push" | "pull") => {
    setTask(dir);
    setLastDir(dir);
    setReport(null);
    setSyncPhase(null);
    if (await doSave()) {
      const taskId = `webdav-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
      taskIdRef.current = taskId;
      try {
        setReport(
          dir === "push"
            ? await webdavPush(lib.id, taskId)
            : await webdavPull(lib.id, taskId),
        );
      } catch (e) {
        onError(String(e));
      }
    }
    setTask(null);
    setSyncPhase(null);
  };

  /** 打断进行中的同步 */
  const onSyncCancel = () => {
    if (taskIdRef.current) void cancelSyncTask(taskIdRef.current);
  };

  const busy = task != null;

  return (
    <div className="webdav-panel">
      <div className="webdav-head">
        <div className="webdav-head-text">
          <div className="webdav-title">WebDav 同步</div>
          <div className="webdav-sub">
            手动推拉双向覆盖（坚果云 / NextCloud 等）：同步到云端 = 本地完整覆盖云端，
            从云端同步 = 云端完整覆盖本地；仅在点击按钮时执行，无自动同步
          </div>
        </div>
        <label className="switch" title={wd.enabled ? "点击停用同步" : "点击启用同步"}>
          <input
            type="checkbox"
            checked={wd.enabled}
            onChange={(e) => patch({ enabled: e.target.checked })}
            disabled={busy}
          />
          <span className="switch-slider" />
        </label>
      </div>

      {wd.enabled && (
        <>
          <label className="modal-label">
            服务器地址
            <input
              value={wd.url}
              placeholder="https://dav.jianguoyun.com/dav/"
              onChange={(e) => patch({ url: e.target.value })}
              disabled={busy}
            />
          </label>
          <div className="webdav-row">
            <label className="modal-label">
              账号
              <input
                value={wd.username}
                onChange={(e) => patch({ username: e.target.value })}
                disabled={busy}
              />
            </label>
            <label className="modal-label">
              密码（应用专用密码）
              <input
                type="password"
                value={wd.password}
                onChange={(e) => patch({ password: e.target.value })}
                disabled={busy}
              />
            </label>
            <label className="modal-label">
              远程目录
              <input
                value={wd.remote_dir}
                placeholder="mystery-novel-agent"
                onChange={(e) => patch({ remote_dir: e.target.value })}
                disabled={busy}
              />
            </label>
          </div>

          {/* 远端位置预览：{远程目录}/{书库名}，同名书库自动对齐匹配 */}
          {wd.url.trim() && /^[A-Za-z0-9_]+$/.test(lib.name) && (
            <div className="form-hint webdav-path">
              远端位置：{wd.url.trim().replace(/\/+$/, "")}
              {wd.remote_dir.trim() ? `/${wd.remote_dir.trim().replace(/^\/+|\/+$/g, "")}` : ""}
              /{lib.name}
              <br />
              同步内容：EPUB + covers 封面 + 数据库（内容一致自动跳过，多出的文件与书籍按覆盖方向删除）
            </div>
          )}

          <div className="webdav-actions">
            <button
              className="btn small"
              onClick={onTest}
              disabled={busy || !wd.url.trim()}
            >
              {task === "test" ? "测试中…" : "测试连接"}
            </button>
            <button
              className="btn small"
              onClick={onSave}
              disabled={busy || !wd.url.trim()}
            >
              {task === "save" ? "保存中…" : "保 存"}
            </button>
            <button
              className="btn small primary"
              onClick={() => void onSync("push")}
              disabled={busy || !wd.url.trim() || !isCurrent}
              title={
                isCurrent
                  ? "先保存当前配置，再把本地书库完整覆盖到云端（云端多出的书籍/封面将被删除）"
                  : "仅允许同步当前书库（请先在「书库」页签切换到该书库）"
              }
            >
              {task === "push" ? "推送中…" : "同步到云端"}
            </button>
            <button
              className="btn small primary"
              onClick={() => void onSync("pull")}
              disabled={busy || !wd.url.trim() || !isCurrent}
              title={
                isCurrent
                  ? "先保存当前配置，再用云端完整覆盖本地书库（本地多出的书籍/封面将被删除）"
                  : "仅允许同步当前书库（请先在「书库」页签切换到该书库）"
              }
            >
              {task === "pull" ? "拉取中…" : "从云端同步"}
            </button>
          </div>

          {/* 同步进度（数量 + 百分比进度条 + 打断） */}
          {(task === "push" || task === "pull") && (
            <div className="notice running batch-bar">
              <div className="batch-info">
                <div className="batch-text">
                  {task === "push" ? "正在同步到云端…" : "正在从云端同步…"}
                  {syncPhase
                    ? ` · ${syncPhase.message}（${syncPhase.current}/${syncPhase.total}，${
                        syncPhase.total > 0
                          ? Math.min(
                              100,
                              Math.round((syncPhase.current / syncPhase.total) * 100),
                            )
                          : 0
                      }%）`
                    : " · 准备中…"}
                </div>
                <div className="progress-bar batch">
                  <div
                    className="progress-fill"
                    style={{
                      width: `${
                        syncPhase && syncPhase.total > 0
                          ? Math.min(
                              100,
                              Math.round((syncPhase.current / syncPhase.total) * 100),
                            )
                          : 0
                      }%`,
                    }}
                  />
                </div>
              </div>
              <button className="btn small danger-btn" onClick={onSyncCancel}>
                打断
              </button>
            </div>
          )}

          {/* 状态行独立于按钮排布，避免出现/消失引起按钮位置跳动 */}
          {testOk === true && <div className="webdav-status ok">✓ 连接成功</div>}
          {testOk === false && <div className="webdav-status bad">✕ 连接失败（详见错误信息）</div>}
          {report && (
            <div className={`webdav-status ${report.failed > 0 ? "bad" : "ok"}`}>
              {lastDir === "push"
                ? `推送完成：本地 ${report.total} 本，上传 ${report.uploaded}，删除云端多余 ${report.deleted}，跳过 ${report.skipped}`
                : `拉取完成：云端 ${report.total} 本，下载 ${report.downloaded}，删除本地多余 ${report.deleted}，跳过 ${report.skipped}`}
              {(report.books_imported > 0 ||
                report.books_updated > 0 ||
                report.books_deleted > 0) &&
                `；数据库：入库 ${report.books_imported}，覆盖 ${report.books_updated}，移除 ${report.books_deleted}`}
              {report.failed > 0 ? `，失败 ${report.failed}` : ""}
              {report.errors.length > 0
                ? `；${report.errors[0]}${
                    report.errors.length > 1
                      ? `（等 ${report.errors.length} 条错误，详见日志）`
                      : ""
                  }`
                : ""}
            </div>
          )}
          <div className="form-hint">
            凭据保存在本机 config.toml；同步会先自动保存当前配置，且仅对当前书库生效。
            「从云端同步」将以云端为准覆盖本地，本地多出的书籍（含短评/书评）会被移除，请谨慎操作。
          </div>
        </>
      )}
    </div>
  );
}

/** 模型添加输入行（回车或按钮添加） */
function ModelInput({ onAdd }: { onAdd: (model: string) => void }) {  const [value, setValue] = useState("");
  const submit = () => {
    if (!value.trim()) return;
    onAdd(value);
    setValue("");
  };
  return (
    <div className="search-row">
      <input
        value={value}
        placeholder="输入模型名，如 gpt-4o-mini…"
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") submit();
        }}
      />
      <button className="btn" onClick={submit} disabled={!value.trim()}>
        添加模型
      </button>
    </div>
  );
}

/** 主题色可配置项 */
const THEME_FIELDS: Array<{ key: keyof LibraryProfile["theme"]; label: string }> = [
  { key: "accent", label: "强调色" },
  { key: "bg", label: "背景" },
  { key: "panel", label: "面板" },
  { key: "panel2", label: "面板浅" },
  { key: "ink", label: "正文" },
  { key: "muted", label: "次要" },
  { key: "read", label: "已读" },
  { key: "reading", label: "在读" },
  { key: "wish", label: "想读" },
];

/** 路径选择弹窗（目录经选择器选取：PC 系统原生 / 移动端内置浏览器） */
function PathChangeDialog({
  libTitle,
  oldPath,
  busy,
  onCancel,
  onConfirm,
}: {
  libTitle: string;
  oldPath: string;
  busy: boolean;
  onCancel: () => void;
  onConfirm: (newPath: string, migrate: boolean) => void;
}) {
  const [newPath, setNewPath] = useState("");
  const [migrate, setMigrate] = useState(true);
  const [picking, setPicking] = useState(false);

  const onPick = async () => {
    setPicking(true);
    const p = await pickDirectory(`选择书库「${libTitle}」的目录`).finally(() =>
      setPicking(false),
    );
    if (p) setNewPath(p);
  };

  return (
    <div className="modal-overlay nested">
      <div className="modal confirm-modal">
        <h2 className="modal-title">修 改 路 径</h2>
        <div className="confirm-message">
          {`书库「${libTitle}」当前路径：\n${oldPath || "（未设置）"}`}
        </div>

        <label className="modal-label">
          <div className="label-row">
            <span>新路径</span>
            <button
              className="link-btn small-link"
              onClick={onPick}
              disabled={busy || picking}
            >
              {picking ? "选择中…" : "选择目录…"}
            </button>
          </div>
          <input value={newPath} readOnly placeholder="尚未选择目录" />
        </label>

        <div className="radio-group">
          <label className="radio">
            <input
              type="radio"
              checked={migrate}
              onChange={() => setMigrate(true)}
              disabled={busy}
            />
            迁移数据到新路径（复制原书库的 EPUB 与封面缓存）
          </label>
          <label className="radio">
            <input
              type="radio"
              checked={!migrate}
              onChange={() => setMigrate(false)}
              disabled={busy}
            />
            直接切换路径（新目录需为空目录，原目录数据保留）
          </label>
        </div>

        <div className="modal-actions">
          <button className="btn" onClick={onCancel} disabled={busy}>
            取消
          </button>
          <button
            className="btn primary"
            disabled={busy || !newPath.trim()}
            onClick={() => onConfirm(newPath.trim(), migrate)}
          >
            {busy ? "处理中…" : "确 认"}
          </button>
        </div>
      </div>
    </div>
  );
}

/** 单个书库配置卡片：标题 / 路径（只读+修改）/ 主题 / 默认标签 */
function LibraryCard({
  draft,
  isDraft,
  current,
  canDelete,
  onPatch,
  onError,
  onSaved,
  onSwitched,
  onDeleted,
}: {
  draft: LibDraft;
  isDraft: boolean;
  current: boolean;
  canDelete: boolean;
  onPatch: (p: Partial<LibDraft>) => void;
  onError: (msg: string | null) => void;
  onSaved: (list: LibraryProfile[], libId: string, newCurrent?: string) => void;
  onSwitched: (newCurrent: string) => void;
  onDeleted: (list: LibraryProfile[]) => void;
}) {
  const { confirm, confirmElement } = useConfirm();
  const [busy, setBusy] = useState(false);
  const [pathDialog, setPathDialog] = useState(false);

  const patchTheme = (key: string, v: string) =>
    onPatch({ theme: { ...draft.theme, [key]: v } });

  const onSave = async () => {
    if (busy) return;
    onError(null);
    // 未配置目录不允许创建/保存
    if (!draft.path.trim()) {
      onError("请先通过「修改路径…」选择书库目录");
      return;
    }
    setBusy(true);
    try {
      if (isDraft) {
        const list = await addLibrary(
          draft.name.trim(),
          draft.title,
          draft.path,
          draft.theme,
          draft.default_tags,
          draft.webdav,
        );
        onSaved(list, draft.id);
      } else {
        const list = await saveLibrary(draft);
        onSaved(list, draft.id);
        // 若保存的是当前书库，即时应用新主题
        if (current) applyTheme(draft.theme);
      }
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const onSwitch = async () => {
    if (busy || current || isDraft) return;
    onError(null);
    setBusy(true);
    try {
      const id = await switchLibrary(draft.id);
      onSwitched(id);
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const onDelete = async () => {
    if (busy) return;
    if (isDraft) {
      onDeleted([]);
      return;
    }
    const ok = await confirm({
      title: "删除书库",
      message: `确定删除书库「${draft.title}」的配置吗？\n\n磁盘上的书库文件不会删除。`,
      okLabel: "删除",
      danger: true,
    });
    if (!ok) return;
    onError(null);
    setBusy(true);
    try {
      const list = await deleteLibrary(draft.id);
      onDeleted(list);
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const onConfirmPath = async (newPath: string, migrate: boolean) => {
    if (isDraft) {
      onPatch({ path: newPath });
      setPathDialog(false);
      return;
    }
    onError(null);
    setBusy(true);
    try {
      await changeLibraryPath(draft.id, newPath, migrate);
      onPatch({ path: newPath });
      setPathDialog(false);
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={`lib-card ${current ? "current" : ""}`}>
      <div className="lib-head">
        {current ? <span className="default-tag">当前</span> : <span className="lib-dot" />}
        {isDraft && <span className="lib-draft-tag">未保存</span>}
      </div>

      <label className="modal-label">
        书库名（仅大小写字母 / 数字 / 下划线，本地唯一；WebDav 远程子目录按其命名）
        <input
          value={draft.name}
          placeholder="my_library"
          onChange={(e) => onPatch({ name: e.target.value })}
          disabled={busy}
          className={draft.name && !/^[A-Za-z0-9_]+$/.test(draft.name) ? "invalid" : ""}
        />
      </label>

      <label className="modal-label">
        标题
        <input
          value={draft.title}
          onChange={(e) => onPatch({ title: e.target.value })}
          disabled={busy}
        />
      </label>

      <label className="modal-label">
        <div className="label-row">
          <span>路径</span>
          <button
            className="link-btn small-link"
            onClick={() => setPathDialog(true)}
            disabled={busy}
          >
            修改路径…
          </button>
        </div>
        <input value={draft.path} readOnly placeholder="尚未设置路径" />
      </label>

      <div className="modal-label">
        主题（点击预设即应用成套配色；亦可用取色器逐项自定义）
        <div className="preset-row">
          {THEME_PRESETS.map((p) => (
            <button
              key={p.name}
              className="preset-chip"
              onClick={() => onPatch({ theme: { ...p.colors } })}
              disabled={busy}
              title={p.name}
            >
              <span className="preset-dot" style={{ background: p.colors.accent ?? "#c9a86a" }} />
              {p.name}
            </button>
          ))}
        </div>
        <div className="color-grid">
          {THEME_FIELDS.map((f) => {
            const v = (draft.theme as Record<string, string | null | undefined>)[f.key as string] ?? "";
            return (
              <div key={f.key} className="color-row">
                <span
                  className="color-block"
                  style={{ background: /^#[0-9a-fA-F]{3,8}$/.test(v) ? v : "transparent" }}
                >
                  <input
                    type="color"
                    className="color-picker"
                    value={/^#[0-9a-fA-F]{6}$/.test(v) ? v : "#000000"}
                    onChange={(e) => patchTheme(f.key as string, e.target.value)}
                    disabled={busy}
                    title={`${f.label}（取色器）`}
                  />
                </span>
                <span className="color-label">{f.label}</span>
                <input
                  className="color-hex"
                  value={v}
                  placeholder="#c9a86a"
                  onChange={(e) => patchTheme(f.key as string, e.target.value)}
                  disabled={busy}
                />
              </div>
            );
          })}
        </div>
      </div>

      <label className="modal-label">
        固定标签（逗号分隔，导入时自动添加）
        <input
          value={draft.default_tags.join(", ")}
          onChange={(e) =>
            onPatch({ default_tags: e.target.value.split(/[,，]/) })
          }
          disabled={busy}
        />
      </label>

      <div className="modal-actions">
        {canDelete ? (
          <button
            className="link-btn small-link danger"
            onClick={onDelete}
            disabled={busy}
          >
            {isDraft ? "放弃" : "删除"}
          </button>
        ) : (
          !isDraft && <span className="form-hint">至少保留一个书库</span>
        )}
        <span className="bulk-flex" />
        {!isDraft && !current && (
          <button className="btn small" onClick={onSwitch} disabled={busy}>
            设为当前
          </button>
        )}
        <button className="btn small primary" onClick={onSave} disabled={busy}>
          {busy ? "处理中…" : isDraft ? "创 建" : "保 存"}
        </button>
      </div>
      {pathDialog && (
        <PathChangeDialog
          libTitle={draft.title}
          oldPath={draft.path}
          busy={busy}
          onCancel={() => setPathDialog(false)}
          onConfirm={onConfirmPath}
        />
      )}
      {confirmElement}
    </div>
  );
}
