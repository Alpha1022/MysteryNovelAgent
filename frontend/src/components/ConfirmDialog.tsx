import { useState } from "react";

export interface ConfirmOptions {
  title: string;
  /** 支持换行（\n） */
  message: string;
  okLabel?: string;
  cancelLabel?: string;
  /** 危险操作（确认按钮红色强调） */
  danger?: boolean;
}

/**
 * 应用内自绘确认弹窗（替代原生 ask，样式与应用一致）
 *
 * 用法：const { confirm, confirmElement } = useConfirm();
 *      if (await confirm({ title: "删除确认", message: "...", danger: true })) { ... }
 *      渲染 {confirmElement}
 */
export function useConfirm() {
  const [state, setState] = useState<
    (ConfirmOptions & { input?: { label: string; value: string }; resolve: (v: string | null) => void }) | null
  >(null);

  const confirm = (opts: ConfirmOptions): Promise<boolean> =>
    new Promise((resolve) => setState({ ...opts, resolve: (value) => resolve(value !== null) }));

  /** 确认前编辑多行文本；取消返回 null。 */
  const confirmText = (opts: ConfirmOptions & { initialValue: string; inputLabel: string }): Promise<string | null> =>
    new Promise((resolve) => setState({
      ...opts, input: { label: opts.inputLabel, value: opts.initialValue }, resolve,
    }));

  const settle = (v: boolean) => {
    state?.resolve(v ? (state.input?.value ?? "") : null);
    setState(null);
  };

  const confirmElement = state ? (
    <div className="modal-overlay confirm-overlay">
      <div className={`modal confirm-modal${state.danger ? " danger" : ""}${state.input ? " confirm-edit-modal" : ""}`}>
        <h2 className="modal-title">{state.title}</h2>
        <div className="confirm-message">{state.message}</div>
        {state.input && (
          <label className="modal-label confirm-input">
            <span>{state.input.label}</span>
            <textarea
              rows={8}
              value={state.input.value}
              onChange={(event) => {
                const value = event.target.value;
                setState((current) => current?.input
                  ? { ...current, input: { ...current.input, value } }
                  : current);
              }}
            />
          </label>
        )}
        <div className="modal-actions">
          <button className="btn" onClick={() => settle(false)} autoFocus>
            {state.cancelLabel ?? "取消"}
          </button>
          <button
            className={`btn ${state.danger ? "danger-btn" : "primary"}`}
            onClick={() => settle(true)}
            disabled={state.input !== undefined && !state.input.value.trim()}
          >
            {state.okLabel ?? "确定"}
          </button>
        </div>
      </div>
    </div>
  ) : null;

  return { confirm, confirmText, confirmElement };
}
