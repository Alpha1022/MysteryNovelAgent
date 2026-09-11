/**
 * 轻量 Markdown 渲染（无外部依赖）：先整体 HTML 转义再按行转换，防注入。
 * 支持：标题 / 有序与无序列表 / 引用 / 围栏代码块 / 行内代码 / 粗体 / 斜体 /
 * http(s) 链接 / 表格（GFM：表头 + `|---|` 分隔行，支持 `:--:` 对齐）。
 */

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/** 行内元素：行内代码 → 粗体 → 斜体 → 链接（输入已转义） */
function inline(s: string): string {
  return s
    .replace(/`([^`]+)`/g, "<code>$1</code>")
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
    .replace(/\*([^*\n]+)\*/g, "<em>$1</em>")
    .replace(
      /\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g,
      '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>',
    );
}

const BLOCK_START =
  /^(#{1,6})\s|^\s*>|^\s*[-*]\s+\S|^\s*\d+[.、]\s+\S|^\s*```|^\s*(-{3,}|\*{3,})\s*$|^\s*\|/;

/** 表格分隔行（GFM）：`| --- | :---: |` 形式 —— 仅由竖线/冒号/短横线/空白组成 */
const TABLE_SEPARATOR = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;

/** 按竖线切分表格行：剥掉首尾管道、去两侧空白；`\|` 转义先占位后还原 */
function splitTableRow(line: string): string[] {
  return line
    .trim()
    .replace(/\\\|/g, "\u0001")
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split("|")
    .map((c) => c.trim().replace(/\u0001/g, "|"));
}

/** 分隔行单元格 → 对齐样式（`:--` 左 / `:--:` 中 / `--:` 右 / `--` 无） */
function alignStyle(cell: string): string {
  const a = cell.trim();
  const left = a.startsWith(":");
  const right = a.endsWith(":");
  if (left && right) return ' style="text-align:center"';
  if (right) return ' style="text-align:right"';
  if (left) return ' style="text-align:left"';
  return "";
}

export function renderMarkdown(src: string): string {
  const lines = src.replace(/\r\n/g, "\n").split("\n");
  const out: string[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];

    // 分隔线（--- / ***）
    if (/^\s*(-{3,}|\*{3,})\s*$/.test(line)) {
      out.push("<hr/>");
      i += 1;
      continue;
    }

    // 围栏代码块
    if (line.trim().startsWith("```")) {
      const buf: string[] = [];
      i += 1;
      while (i < lines.length && !lines[i].trim().startsWith("```")) {
        buf.push(lines[i]);
        i += 1;
      }
      i += 1; // 跳过收尾 ```
      out.push(`<pre><code>${escapeHtml(buf.join("\n"))}</code></pre>`);
      continue;
    }

    // 表格（GFM）：当前行含竖线且下一行是 `|---|` 分隔行
    if (
      line.includes("|") &&
      i + 1 < lines.length &&
      TABLE_SEPARATOR.test(lines[i + 1]) &&
      /\|/.test(lines[i + 1])
    ) {
      const header = splitTableRow(line);
      const aligns = splitTableRow(lines[i + 1]);
      i += 2;
      const body: string[][] = [];
      while (i < lines.length && lines[i].includes("|")) {
        body.push(splitTableRow(lines[i]));
        i += 1;
      }
      const th = header
        .map(
          (c, idx) =>
            `<th${alignStyle(aligns[idx] ?? "")}>${inline(escapeHtml(c))}</th>`,
        )
        .join("");
      const trs = body
        .map(
          (r) =>
            `<tr>${r
              .map(
                (c, idx) =>
                  `<td${alignStyle(aligns[idx] ?? "")}>${inline(
                    escapeHtml(c),
                  )}</td>`,
              )
              .join("")}</tr>`,
        )
        .join("");
      out.push(
        `<div class="table-wrap"><table><thead><tr>${th}</tr></thead><tbody>${trs}</tbody></table></div>`,
      );
      continue;
    }

    // 标题
    const h = /^(#{1,6})\s+(.*)$/.exec(line);
    if (h) {
      const level = h[1].length;
      out.push(`<h${level}>${inline(escapeHtml(h[2]))}</h${level}>`);
      i += 1;
      continue;
    }

    // 引用
    if (/^\s*>\s?/.test(line)) {
      const buf: string[] = [];
      while (i < lines.length && /^\s*>\s?/.test(lines[i])) {
        buf.push(lines[i].replace(/^\s*>\s?/, ""));
        i += 1;
      }
      out.push(`<blockquote>${renderMarkdown(buf.join("\n"))}</blockquote>`);
      continue;
    }

    // 无序列表（项间允许空行，避免被拆成多个列表）
    if (/^\s*[-*]\s+\S/.test(line)) {
      const buf: string[] = [];
      while (i < lines.length) {
        if (/^\s*[-*]\s+\S/.test(lines[i])) {
          buf.push(lines[i].replace(/^\s*[-*]\s+/, ""));
          i += 1;
        } else if (
          lines[i].trim() === "" &&
          i + 1 < lines.length &&
          /^\s*[-*]\s+\S/.test(lines[i + 1])
        ) {
          i += 1; // 跳过列表项之间的空行
        } else {
          break;
        }
      }
      out.push(`<ul>${buf.map((b) => `<li>${inline(escapeHtml(b))}</li>`).join("")}</ul>`);
      continue;
    }

    // 有序列表（兼容中文顿号；项间允许空行）
    if (/^\s*\d+[.、]\s+\S/.test(line)) {
      const buf: string[] = [];
      while (i < lines.length) {
        if (/^\s*\d+[.、]\s+\S/.test(lines[i])) {
          buf.push(lines[i].replace(/^\s*\d+[.、]\s+/, ""));
          i += 1;
        } else if (
          lines[i].trim() === "" &&
          i + 1 < lines.length &&
          /^\s*\d+[.、]\s+\S/.test(lines[i + 1])
        ) {
          i += 1; // 跳过列表项之间的空行
        } else {
          break;
        }
      }
      out.push(`<ol>${buf.map((b) => `<li>${inline(escapeHtml(b))}</li>`).join("")}</ol>`);
      continue;
    }

    // 空行
    if (line.trim() === "") {
      i += 1;
      continue;
    }

    // 普通段落（连续非空、非块首的行）
    const buf: string[] = [];
    while (i < lines.length && lines[i].trim() !== "" && !BLOCK_START.test(lines[i])) {
      buf.push(lines[i]);
      i += 1;
    }
    out.push(`<p>${buf.map((b) => inline(escapeHtml(b))).join("<br/>")}</p>`);
  }

  return out.join("\n");
}
