import { useEffect, useState } from "react";
import { useFileSrc } from "./fileSrc";

interface Props {
  coverPath: string | null;
  title: string;
}

/** 本地缓存封面：经后端命令读取字节渲染（移动端不依赖 asset 协议） */
export default function CoverImage({ coverPath, title }: Props) {
  const src = useFileSrc(coverPath);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    setFailed(false);
  }, [coverPath]);

  if (!src || failed) {
    return <div className="cover placeholder">{title}</div>;
  }

  return (
    <img
      className="cover"
      src={src}
      alt={title}
      loading="lazy"
      onError={() => setFailed(true)}
    />
  );
}
