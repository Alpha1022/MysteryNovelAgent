export default function SearchBar({
  value,
  onChange,
}: {
  value: string;
  onChange: (v: string) => void;
}) {
  return (
    <div className="search">
      <input
        type="text"
        placeholder="搜索书名或作者…"
        value={value}
        onChange={(e) => onChange(e.target.value)}
      />
    </div>
  );
}
