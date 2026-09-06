export default function StateView({
  kind,
  message,
}: {
  kind: "loading" | "error" | "empty";
  message?: string;
}) {
  if (kind === "loading") {
    return <div className="state">加载中…</div>;
  }
  if (kind === "error") {
    return <div className="state error">加载失败：{message}</div>;
  }
  return (
    <div className="state">
      <div>书库还是空的</div>
    </div>
  );
}
