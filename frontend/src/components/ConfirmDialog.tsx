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
    (ConfirmOptions & { resolve: (v: boolean) => void }) | null
  >(null);

  const confirm = (opts: ConfirmOptions): Promise<boolean> =>
    new Promise((resolve) => setState({ ...opts, resolve }));

  const settle = (v: boolean) => {
    state?.resolve(v);
    setState(null);
  };

  const confirmElement = state ? (
    <div className="modal-overlay confirm-overlay">
      <div className={`modal confirm-modal${state.danger ? " danger" : ""}`}>
        <h2 className="modal-title">{state.title}</h2>
        <div className="confirm-message">{state.message}</div>
        <div className="modal-actions">
          <button className="btn" onClick={() => settle(false)} autoFocus>
            {state.cancelLabel ?? "取消"}
          </button>
          <button
            className={`btn ${state.danger ? "danger-btn" : "primary"}`}
            onClick={() => settle(true)}
          >
            {state.okLabel ?? "确定"}
          </button>
        </div>
      </div>
    </div>
  ) : null;

  return { confirm, confirmElement };
}
