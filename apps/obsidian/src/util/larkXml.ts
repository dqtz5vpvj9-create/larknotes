/**
 * Convert a Lark docx **v2 XML** document into Obsidian-flavoured Markdown.
 *
 * Why XML and not the v2 `--doc-format markdown` output: the markdown path is
 * lossless for text, but it renders images as `![](signed-url)` — a
 * short-lived URL with no stable token. The XML path keeps `<img src="…">`
 * where `src` is the durable Lark file_token, which is what the attachment
 * cache needs to download the asset and produce a stable `![[token.ext]]`
 * embed. So the pull path fetches XML and converts it here.
 *
 * The XML is a small, HTML-like dialect. Vocabulary handled (probed against
 * lark-cli 1.0.30): `<title> <p> <h1>…<h9> <b> <em>/<i> <code> <a href>
 * <ul>/<ol>/<li> <blockquote> <pre lang><code> <br/> <table>/<thead>/<tbody>/
 * <tr>/<th>/<td> <colgroup>/<col> <hr/> <latex> <img src>`. Unknown tags
 * fall back to converting their children, so a new block type degrades to its
 * text content rather than throwing.
 *
 * Push still goes out as v2 markdown (see obsidianToLarkMd.ts) — the two-axis
 * sync state (localBaseHash vs. remoteEditTime) tolerates the format split,
 * because the remote baseline is `obj_edit_time`, never a content hash.
 */
import { DOMParser } from "@xmldom/xmldom";

export interface ConvertOptions {
  /** Lark image file_token → vault filename in `_attachments/`. */
  imageMap?: Record<string, string>;
  /** Lark `node_token` → resolved doc title, for rewriting intra-wiki links
   *  into Obsidian `[[wikilinks]]`. */
  nodeTitleMap?: Record<string, string>;
}

const ELEMENT_NODE = 1;
const TEXT_NODE = 3;

const LARK_HOST_RE =
  /(?:[a-z0-9-]+\.)*(?:feishu\.cn|feishu\.com|larksuite\.com|larkoffice\.com)/i;

/** Parse the doc XML (a flat sequence of block elements, no single root) and
 *  emit Obsidian Markdown. */
export function larkXmlToObsidianMarkdown(xml: string, opts: ConvertOptions = {}): string {
  if (!xml || xml.trim() === "") return "";
  const doc = new DOMParser({
    // The doc XML is trusted lark-cli output; silence the noisy default
    // console error handler so a stray entity doesn't spam the dev console.
    onError: () => {},
  }).parseFromString(`<root>${xml}</root>`, "text/xml");
  const root = doc.documentElement;
  if (!root) return "";

  const blocks: string[] = [];
  for (let i = 0; i < root.childNodes.length; i++) {
    const md = convertBlock(root.childNodes[i] as any, opts);
    if (md !== null && md !== "") blocks.push(md);
  }
  return blocks.join("\n\n").trim() + "\n";
}

/** Collect every `src` token from `<img>` elements, in document order. */
export function extractImageTokens(xml: string): string[] {
  if (!xml) return [];
  const doc = new DOMParser({ onError: () => {} }).parseFromString(
    `<root>${xml}</root>`,
    "text/xml",
  );
  const tokens = new Set<string>();
  const images = doc.getElementsByTagName("img");
  for (let i = 0; i < images.length; i++) {
    const src = images[i].getAttribute("src");
    if (src) tokens.add(src);
  }
  return [...tokens];
}

// ─── block-level ────────────────────────────────────────────────────────────

function convertBlock(node: any, opts: ConvertOptions): string | null {
  if (node.nodeType === TEXT_NODE) {
    // Loose text between block elements — Lark doesn't emit it, but be safe.
    const t = (node.nodeValue ?? "").trim();
    return t ? escapeText(t) : null;
  }
  if (node.nodeType !== ELEMENT_NODE) return null;

  const tag = (node.nodeName as string).toLowerCase();

  if (tag === "title") return `# ${inlineMd(node, opts)}`;
  if (/^h[1-9]$/.test(tag)) return `${"#".repeat(Number(tag[1]))} ${inlineMd(node, opts)}`;

  switch (tag) {
    case "p": {
      // A <p> whose only child is one <latex> is a block equation.
      const only = soleElementChild(node);
      if (only && only.nodeName.toLowerCase() === "latex") {
        return `$$${textOf(only)}$$`;
      }
      return inlineMd(node, opts);
    }
    case "ul":
      return convertList(node, opts, false, 0);
    case "ol":
      return convertList(node, opts, true, 0);
    case "blockquote": {
      // Children are usually <p>; render each as a block, prefix every line.
      const inner: string[] = [];
      for (let i = 0; i < node.childNodes.length; i++) {
        const md = convertBlock(node.childNodes[i], opts);
        if (md !== null && md !== "") inner.push(md);
      }
      return inner
        .join("\n\n")
        .split("\n")
        .map((l) => (l ? `> ${l}` : ">"))
        .join("\n");
    }
    case "pre": {
      const lang = node.getAttribute("lang") || "";
      const code = node.getElementsByTagName("code")[0];
      const body = code ? rawTextWithBreaks(code) : rawTextWithBreaks(node);
      return `\`\`\`${lang}\n${body}\n\`\`\``;
    }
    case "table":
      return convertTable(node, opts);
    case "hr":
      return "---";
    case "img":
      return convertImage(node, opts);
    case "colgroup":
      return null; // table column metadata — consumed by convertTable
    default:
      // Unknown block: degrade to its inline content rather than dropping it.
      return inlineMd(node, opts);
  }
}

function convertList(
  listEl: any,
  opts: ConvertOptions,
  ordered: boolean,
  depth: number,
): string {
  const indent = "  ".repeat(depth);
  const lines: string[] = [];
  let idx = 1;
  for (let i = 0; i < listEl.childNodes.length; i++) {
    const li = listEl.childNodes[i];
    if (li.nodeType !== ELEMENT_NODE || li.nodeName.toLowerCase() !== "li") continue;

    let inlineParts = "";
    let nested = "";
    for (let j = 0; j < li.childNodes.length; j++) {
      const c = li.childNodes[j];
      const childTag =
        c.nodeType === ELEMENT_NODE ? (c.nodeName as string).toLowerCase() : "";
      if (childTag === "ul" || childTag === "ol") {
        nested += "\n" + convertList(c, opts, childTag === "ol", depth + 1);
      } else {
        inlineParts += inlineOfNode(c, opts);
      }
    }
    const marker = ordered ? `${idx}. ` : "- ";
    lines.push(`${indent}${marker}${inlineParts.trim()}${nested}`);
    idx++;
  }
  return lines.join("\n");
}

function convertTable(tableEl: any, opts: ConvertOptions): string {
  const rows: string[][] = [];
  const trs = tableEl.getElementsByTagName("tr");
  for (let i = 0; i < trs.length; i++) {
    const cells: string[] = [];
    const tr = trs[i];
    for (let j = 0; j < tr.childNodes.length; j++) {
      const cell = tr.childNodes[j];
      if (cell.nodeType !== ELEMENT_NODE) continue;
      const cellTag = (cell.nodeName as string).toLowerCase();
      if (cellTag !== "td" && cellTag !== "th") continue;
      cells.push(cellMd(cell, opts));
    }
    if (cells.length > 0) rows.push(cells);
  }
  if (rows.length === 0) return "";

  const width = Math.max(...rows.map((r) => r.length));
  const pad = (r: string[]) => [...r, ...Array(width - r.length).fill("")];

  // GFM requires a header row; promote row 0 (Lark tables nearly always have
  // a <thead>, and this matches the old v1 converter's behaviour).
  const out: string[] = [];
  out.push(`| ${pad(rows[0]).join(" | ")} |`);
  out.push(`| ${Array(width).fill("---").join(" | ")} |`);
  for (const r of rows.slice(1)) out.push(`| ${pad(r).join(" | ")} |`);
  return out.join("\n");
}

/** A table cell holds `<p>` blocks; join them with `<br>`, escape pipes. */
function cellMd(cell: any, opts: ConvertOptions): string {
  const parts: string[] = [];
  for (let i = 0; i < cell.childNodes.length; i++) {
    const c = cell.childNodes[i];
    if (c.nodeType === ELEMENT_NODE && c.nodeName.toLowerCase() === "p") {
      parts.push(inlineMd(c, opts));
    } else {
      parts.push(inlineOfNode(c, opts));
    }
  }
  return parts
    .join(" <br> ")
    .replace(/\|/g, "\\|")
    .replace(/\n/g, " ")
    .trim();
}

function convertImage(node: any, opts: ConvertOptions): string {
  const token = node.getAttribute("src") || "";
  const mapped = token ? opts.imageMap?.[token] : undefined;
  if (mapped) return `![[${mapped}]]`;
  // Not in the attachment cache (download failed or not yet run). Fall back
  // to the signed URL so the image at least renders once; it will expire.
  const href = node.getAttribute("href");
  if (href) return `![](${href})`;
  return token ? `*[📷 image — Lark token \`${token}\`]*` : "*[📷 image]*";
}

// ─── inline ─────────────────────────────────────────────────────────────────

/** Concatenate the inline markdown of an element's children. */
function inlineMd(el: any, opts: ConvertOptions): string {
  let out = "";
  for (let i = 0; i < el.childNodes.length; i++) {
    out += inlineOfNode(el.childNodes[i], opts);
  }
  return out;
}

function inlineOfNode(node: any, opts: ConvertOptions): string {
  if (node.nodeType === TEXT_NODE) return escapeText(node.nodeValue ?? "");
  if (node.nodeType !== ELEMENT_NODE) return "";

  const tag = (node.nodeName as string).toLowerCase();
  switch (tag) {
    case "b":
    case "strong":
      return `**${inlineMd(node, opts)}**`;
    case "em":
    case "i":
      return `*${inlineMd(node, opts)}*`;
    case "code":
      return `\`${textOf(node)}\``;
    case "s":
    case "del":
      return `~~${inlineMd(node, opts)}~~`;
    case "br":
      return "\n";
    case "latex":
      return `$${textOf(node)}$`;
    case "img":
      return convertImage(node, opts);
    case "a": {
      const href = node.getAttribute("href") || "";
      const label = inlineMd(node, opts);
      const token = wikiTokenFromUrl(href);
      const title = token ? opts.nodeTitleMap?.[token] : undefined;
      if (title) {
        return label.trim() === title ? `[[${title}]]` : `[[${title}|${label}]]`;
      }
      return `[${label}](${href})`;
    }
    default:
      // Unknown inline wrapper (e.g. <text> styling) — keep the content.
      return inlineMd(node, opts);
  }
}

// ─── helpers ────────────────────────────────────────────────────────────────

/** Plain text content of a node, entities already decoded by the parser. */
function textOf(node: any): string {
  let out = "";
  for (let i = 0; i < node.childNodes.length; i++) {
    const c = node.childNodes[i];
    if (c.nodeType === TEXT_NODE) out += c.nodeValue ?? "";
    else if (c.nodeType === ELEMENT_NODE) out += textOf(c);
  }
  return out;
}

/** Like textOf, but `<br/>` becomes a real newline — for code blocks. */
function rawTextWithBreaks(node: any): string {
  let out = "";
  for (let i = 0; i < node.childNodes.length; i++) {
    const c = node.childNodes[i];
    if (c.nodeType === TEXT_NODE) {
      out += c.nodeValue ?? "";
    } else if (c.nodeType === ELEMENT_NODE) {
      if ((c.nodeName as string).toLowerCase() === "br") out += "\n";
      else out += rawTextWithBreaks(c);
    }
  }
  return out;
}

/** The single element child of `el`, ignoring whitespace text — else null. */
function soleElementChild(el: any): any | null {
  let found: any = null;
  for (let i = 0; i < el.childNodes.length; i++) {
    const c = el.childNodes[i];
    if (c.nodeType === TEXT_NODE && (c.nodeValue ?? "").trim() === "") continue;
    if (found) return null; // more than one meaningful child
    if (c.nodeType !== ELEMENT_NODE) return null;
    found = c;
  }
  return found;
}

/** Extract a wiki `node_token` from a Lark wiki URL, else null. */
function wikiTokenFromUrl(url: string): string | null {
  const m = new RegExp(
    `^https?://${LARK_HOST_RE.source}/wiki/([A-Za-z0-9]+)`,
    "i",
  ).exec(url);
  return m ? m[1] : null;
}

/** Escape the Markdown metacharacters that would otherwise reinterpret
 *  literal text. Backslash first so we don't double-escape our own escapes. */
function escapeText(text: string): string {
  return text.replace(/([\\`*_[\]<>])/g, "\\$1");
}
