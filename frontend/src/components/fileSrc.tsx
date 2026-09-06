import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/** 本地封面 blob URL 缓存（会话级，避免重复读取） */
const cache = new Map<string, string>();

function mimeOf(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  switch (ext) {
    case "png":
      return "image/png";
    case "webp":
      return "image/webp";
    case "gif":
      return "image/gif";
    default:
      return "image/jpeg";
  }
}

/**
 * 本地封面文件 → blob URL（经后端 read_cover_file 读取）。
 *
 * 不用 convertFileSrc/asset 协议：Android WebView 下 asset 协议对
 * 绝对路径不可靠（同步后的封面不显示即此因），命令通道全平台一致。
 */
export function useFileSrc(path: string | null | undefined): string | null {
  const [src, setSrc] = useState<string | null>(() =>
    path ? (cache.get(path) ?? null) : null,
  );

  useEffect(() => {
    if (!path) {
      setSrc(null);
      return;
    }
    const cached = cache.get(path);
    if (cached) {
      setSrc(cached);
      return;
    }
    setSrc(null);
    let cancelled = false;
    invoke<ArrayBuffer>("read_cover_file", { path })
      .then((buf) => {
        const objectUrl = URL.createObjectURL(
          new Blob([buf], { type: mimeOf(path) }),
        );
        cache.set(path, objectUrl);
        if (!cancelled) setSrc(objectUrl);
      })
      .catch(() => {
        if (!cancelled) setSrc(null);
      });
    return () => {
      cancelled = true;
    };
  }, [path]);

  return src;
}

/** 本地封面图片：路径加载完成前不渲染（父级可自行放置占位） */
export function FileImg({
  path,
  className,
  alt,
  lazy,
}: {
  path: string;
  className?: string;
  alt: string;
  lazy?: boolean;
}) {
  const src = useFileSrc(path);
  if (!src) return null;
  return (
    <img
      className={className}
      src={src}
      alt={alt}
      draggable={false}
      loading={lazy ? "lazy" : undefined}
    />
  );
}
