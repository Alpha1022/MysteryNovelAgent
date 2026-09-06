import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/** 远程封面 blob URL 缓存（会话级，避免重复请求防盗链图床） */
const cache = new Map<string, string>();

interface Props {
  url: string;
  title: string;
  className: string;
}

/**
 * 远程封面：经后端代理下载（伪装 Referer 过 OSS/豆瓣防盗链），
 * 以 blob URL 渲染；加载失败降级为文字占位
 */
export default function RemoteCover({ url, title, className }: Props) {
  const [src, setSrc] = useState<string | null>(() => cache.get(url) ?? null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    setFailed(false);
    const cached = cache.get(url);
    if (cached) {
      setSrc(cached);
      return;
    }
    setSrc(null);
    let cancelled = false;
    invoke<ArrayBuffer>("fetch_cover_image", { url })
      .then((buf) => {
        const objectUrl = URL.createObjectURL(
          new Blob([buf], { type: "image/jpeg" }),
        );
        cache.set(url, objectUrl);
        if (!cancelled) setSrc(objectUrl);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [url]);

  if (!src || failed) {
    return <div className={`${className} placeholder`}>{title}</div>;
  }

  return <img className={className} src={src} alt={title} loading="lazy" />;
}
