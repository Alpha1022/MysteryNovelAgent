import { useRef, useState, type PointerEvent as ReactPointerEvent, type WheelEvent as ReactWheelEvent } from "react";

/** 封面展示比例（全局统一 2:3，见 global.css 各 .cover 的 aspect-ratio） */
const RATIO = 2 / 3;
/** 裁剪视口尺寸（px；输出按原图分辨率裁剪，不受此尺寸限制） */
const VIEW_W = 240;
const VIEW_H = Math.round(VIEW_W / RATIO);
/** 缩放上限（相对 cover 基准的倍数） */
const MAX_ZOOM = 6;

interface Props {
  /** 原图 objectURL（组件负责在卸载/更换时回收） */
  src: string;
  /** 源为 PNG 时输出 PNG（保留透明），否则输出 JPEG */
  isPng: boolean;
  /** 上传执行中（禁用按钮） */
  busy: boolean;
  /** 放弃本次选择 */
  onCancel: () => void;
  /** 不裁剪，直接使用原图 */
  onUseOriginal: () => void;
  /** 确认截取（裁剪结果；比例恒为 2:3，分辨率取原图对应区域） */
  onConfirm: (blob: Blob) => void;
}

/**
 * 封面截取：固定 2:3 视口（与书架/详情页展示比例一致），图片在其下
 * 拖动平移、滚轮/滑杆缩放（cover 模式，始终铺满视口）。
 * 确认时按原图分辨率从源图抠取视口对应区域（不放大）。
 */
export default function CoverCrop({ src, isPng, busy, onCancel, onUseOriginal, onConfirm }: Props) {
  const [natural, setNatural] = useState({ w: 0, h: 0 });
  const [scale, setScale] = useState(1);
  const [offset, setOffset] = useState({ x: 0, y: 0 });
  const imgRef = useRef<HTMLImageElement>(null);
  const viewRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ px: number; py: number; ox: number; oy: number } | null>(null);

  /** cover 基准缩放（图片恰好铺满视口） */
  const minScale = natural.w > 0 ? Math.max(VIEW_W / natural.w, VIEW_H / natural.h) : 1;

  const clampOffset = (x: number, y: number, s: number) => ({
    x: Math.min(0, Math.max(VIEW_W - natural.w * s, x)),
    y: Math.min(0, Math.max(VIEW_H - natural.h * s, y)),
  });

  const onImgLoad = () => {
    const img = imgRef.current;
    if (!img) return;
    const w = img.naturalWidth;
    const h = img.naturalHeight;
    setNatural({ w, h });
    // 初始：cover 居中
    const s = Math.max(VIEW_W / w, VIEW_H / h);
    setScale(s);
    setOffset({ x: (VIEW_W - w * s) / 2, y: (VIEW_H - h * s) / 2 });
  };

  /** 以视口内 (cx, cy) 为锚点缩放（保持该点对应的图片位置不动） */
  const zoomAt = (cx: number, cy: number, nextScale: number) => {
    const ns = Math.min(minScale * MAX_ZOOM, Math.max(minScale, nextScale));
    const nx = cx - ((cx - offset.x) * ns) / scale;
    const ny = cy - ((cy - offset.y) * ns) / scale;
    setScale(ns);
    setOffset(clampOffset(nx, ny, ns));
  };

  const onWheel = (e: ReactWheelEvent) => {
    e.preventDefault();
    const rect = viewRef.current?.getBoundingClientRect();
    if (!rect) return;
    const factor = e.deltaY < 0 ? 1.12 : 1 / 1.12;
    zoomAt(e.clientX - rect.left, e.clientY - rect.top, scale * factor);
  };

  const onPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    dragRef.current = { px: e.clientX, py: e.clientY, ox: offset.x, oy: offset.y };
  };
  const onPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const d = dragRef.current;
    if (!d) return;
    setOffset(clampOffset(d.ox + (e.clientX - d.px), d.oy + (e.clientY - d.py), scale));
  };
  const onPointerUp = () => {
    dragRef.current = null;
  };

  const onConfirmClick = () => {
    const img = imgRef.current;
    if (!img || natural.w === 0) return;
    // 视口 → 原图像素坐标
    const sx = -offset.x / scale;
    const sy = -offset.y / scale;
    const sw = VIEW_W / scale;
    const sh = VIEW_H / scale;
    if (sw < 1 || sh < 1) return;
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.round(sw));
    canvas.height = Math.max(1, Math.round(sh));
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.drawImage(img, sx, sy, sw, sh, 0, 0, canvas.width, canvas.height);
    canvas.toBlob(
      (blob) => {
        if (blob) onConfirm(blob);
      },
      isPng ? "image/png" : "image/jpeg",
      isPng ? undefined : 0.92,
    );
  };

  return (
    <div className="crop-panel">
      <div className="crop-hint">
        拖动调整位置 · 滚轮或滑杆缩放（虚线区域 = 封面展示比例 2:3）
      </div>
      <div className="crop-stage">
        <div
          className="crop-view"
          ref={viewRef}
          onWheel={onWheel}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
        >
          <img
            ref={imgRef}
            src={src}
            alt="待截取封面"
            draggable={false}
            onLoad={onImgLoad}
            style={{
              width: natural.w * scale,
              height: natural.h * scale,
              left: offset.x,
              top: offset.y,
            }}
          />
        </div>
        <input
          className="crop-zoom"
          type="range"
          min={minScale}
          max={minScale * MAX_ZOOM}
          step={(minScale * (MAX_ZOOM - 1)) / 200}
          value={scale}
          onChange={(e) => {
            const rect = viewRef.current?.getBoundingClientRect();
            zoomAt(
              (rect?.width ?? VIEW_W) / 2,
              (rect?.height ?? VIEW_H) / 2,
              Number(e.target.value),
            );
          }}
          disabled={busy || natural.w === 0}
          title="缩放"
        />
      </div>
      <div className="crop-actions">
        <button className="btn" onClick={onCancel} disabled={busy}>
          取消
        </button>
        <button className="btn" onClick={onUseOriginal} disabled={busy}>
          使用原图
        </button>
        <button
          className="btn primary"
          onClick={onConfirmClick}
          disabled={busy || natural.w === 0}
          title="按 2:3 截取视口区域并上传"
        >
          {busy ? "上传中…" : "确认截取"}
        </button>
      </div>
    </div>
  );
}
