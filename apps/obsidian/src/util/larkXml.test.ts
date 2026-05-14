import { describe, expect, test } from "vitest";
import { extractImageTokens, larkXmlToObsidianMarkdown } from "./larkXml";

describe("larkXmlToObsidianMarkdown", () => {
  test("full rich-vocabulary document", () => {
    // Verbatim `docs +fetch --doc-format xml --detail with-ids` output
    // (lark-cli 1.0.30) for a doc exercising every probed block type.
    const xml =
      `<title id="Z7y">Rich Vocab Probe</title>` +
      `<p id="p1">A paragraph with <b>bold</b>, <em>italic</em>, <code>inline code</code> and a <a href="https://example.com">link</a>.</p>` +
      `<h2 id="h">Heading 2</h2>` +
      `<ul><li id="l1">bullet one</li><li id="l2">bullet two<ul><li id="l3">nested</li></ul></li></ul>` +
      `<ol><li id="o1">first</li><li id="o2">second</li></ol>` +
      `<blockquote id="bq"><p id="qp">a quote</p></blockquote>` +
      `<pre id="pre" caption="&#xA;" lang="python"><code>def f(x):<br/>    return x_1 + x_2</code></pre>` +
      `<table id="t"><colgroup><col width="120"/><col width="120"/></colgroup>` +
      `<thead><tr><th vertical-align="top"><p id="ca">Col A</p></th><th vertical-align="top"><p id="cb">Col B</p></th></tr></thead>` +
      `<tbody><tr><td vertical-align="top"><p id="a1">a1</p></td><td vertical-align="top"><p id="b2">b2</p></td></tr></tbody></table>` +
      `<hr id="hr"/>` +
      `<p id="p2">Inline math <latex>a_{x}</latex> and done.</p>`;

    const expected = [
      "# Rich Vocab Probe",
      "",
      "A paragraph with **bold**, *italic*, `inline code` and a [link](https://example.com).",
      "",
      "## Heading 2",
      "",
      "- bullet one",
      "- bullet two",
      "  - nested",
      "",
      "1. first",
      "2. second",
      "",
      "> a quote",
      "",
      "```python",
      "def f(x):",
      "    return x_1 + x_2",
      "```",
      "",
      "| Col A | Col B |",
      "| --- | --- |",
      "| a1 | b2 |",
      "",
      "---",
      "",
      "Inline math $a_{x}$ and done.",
      "",
    ].join("\n");

    expect(larkXmlToObsidianMarkdown(xml)).toBe(expected);
  });

  test("block math: a <p> wrapping a lone <latex> becomes $$…$$", () => {
    expect(larkXmlToObsidianMarkdown(`<p><latex>x_{i} = \\sum y_{j}</latex></p>`)).toBe(
      "$$x_{i} = \\sum y_{j}$$\n",
    );
  });

  test("inline math stays $…$ and is not escaped", () => {
    expect(larkXmlToObsidianMarkdown(`<p>see <latex>a_{x}</latex> ok</p>`)).toBe(
      "see $a_{x}$ ok\n",
    );
  });

  test("markdown metacharacters in prose are escaped", () => {
    expect(larkXmlToObsidianMarkdown(`<p>literal *stars* and _under_</p>`)).toBe(
      "literal \\*stars\\* and \\_under\\_\n",
    );
  });

  test("image with a known token → ![[file]]", () => {
    const xml = `<p>before <img src="TOK123" name="x.png"/> after</p>`;
    expect(
      larkXmlToObsidianMarkdown(xml, { imageMap: { TOK123: "TOK123.png" } }),
    ).toBe("before ![[TOK123.png]] after\n");
  });

  test("image with no cache entry falls back to its signed href", () => {
    const xml = `<p><img src="TOK" href="https://drive.example/authcode?code=abc"/></p>`;
    expect(larkXmlToObsidianMarkdown(xml)).toBe(
      "![](https://drive.example/authcode?code=abc)\n",
    );
  });

  test("wiki link → [[wikilink]] when the node token is known", () => {
    const xml = `<p><a href="https://foo.feishu.cn/wiki/ABC123">My Doc</a></p>`;
    expect(
      larkXmlToObsidianMarkdown(xml, { nodeTitleMap: { ABC123: "My Doc" } }),
    ).toBe("[[My Doc]]\n");
  });

  test("wiki link keeps the alias when label ≠ title", () => {
    const xml = `<p><a href="https://foo.feishu.cn/wiki/ABC123">see here</a></p>`;
    expect(
      larkXmlToObsidianMarkdown(xml, { nodeTitleMap: { ABC123: "My Doc" } }),
    ).toBe("[[My Doc|see here]]\n");
  });

  test("unknown wiki token is left as a plain link", () => {
    const xml = `<p><a href="https://foo.feishu.cn/wiki/UNKNOWN">x</a></p>`;
    expect(larkXmlToObsidianMarkdown(xml)).toBe(
      "[x](https://foo.feishu.cn/wiki/UNKNOWN)\n",
    );
  });

  test("empty input → empty string", () => {
    expect(larkXmlToObsidianMarkdown("")).toBe("");
  });
});

describe("extractImageTokens", () => {
  test("collects every <img> src in document order, deduped", () => {
    const xml =
      `<p><img src="A"/></p><p>text</p><p><img src="B"/><img src="A"/></p>`;
    expect(extractImageTokens(xml)).toEqual(["A", "B"]);
  });

  test("no images → empty array", () => {
    expect(extractImageTokens(`<p>just text</p>`)).toEqual([]);
  });
});
