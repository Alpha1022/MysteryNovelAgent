import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/** EPUB 内嵌封面 blob URL 缓存（会话级，避免重复提取） */
const cache = new Map<string, string>();

interface Props {
  path: string;
  title: string;
  className: string;
}

/**
 * EPUB 内嵌封面预览：经后端提取字节，以 blob URL 渲染；
 * 提取失败降级为文字占位
 */
export default function EpubCover({ path, title, className }: Props) {
  const [src, setSrc] = useState<string | null>(() => cache.get(path) ?? null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    setFailed(false);
    const cached = cache.get(path);
    if (cached) {
      setSrc(cached);
      return;
    }
    setSrc(null);
    let cancelled = false;
    invoke<ArrayBuffer>("fetch_epub_cover", { path })
      .then((buf) => {
        const objectUrl = URL.createObjectURL(
          new Blob([buf], { type: "image/jpeg" }),
        );
        cache.set(path, objectUrl);
        if (!cancelled) setSrc(objectUrl);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [path]);

  if (!src || failed) {
    return <div className={`${className} placeholder`}>{title}</div>;
  }

  return <img className={className} src={src} alt={title} />;
}
