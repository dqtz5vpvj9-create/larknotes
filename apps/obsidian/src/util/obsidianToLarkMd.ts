/**
 * Pre-process vault markdown before pushing it to Lark (docx **v2** API).
 *
 * Since the v2 switch this is a near-identity passthrough. v2's
 * `docs +update --doc-format markdown` accepts standard GFM directly —
 * pipe tables, fenced code, `$...$` math all round-trip losslessly — so the
 * v1-era rewrites this module used to do are not just unnecessary but
 * *harmful*:
 *
 *   - GFM pipe tables → `<lark-table>` :  v2 wants the GFM pipes as-is;
 *                                         injecting `<lark-table>` HTML would
 *                                         land as literal text.
 *   - `![[token.ext]]` → `<image token>`: v2's markdown parser doesn't
 *                                         understand `<image>` tags.
 *
 * Both are therefore gone. The function and its options are kept so the push
 * path in SyncEngine has a stable seam for future Obsidian→Lark transforms
 * (e.g. resolving `[[wikilinks]]` back to Lark wiki URLs).
 */

export interface InverseOptions {
  /**
   * Unused since the v2 switch — kept for call-site stability. Pull-side
   * image sync lives in larkXml.ts; pushing local images back is still a
   * follow-up (the markdown push path can't carry a file token).
   */
  knownImageFilenames?: Set<string>;
}

export function obsidianToLarkMarkdown(src: string, _opts: InverseOptions = {}): string {
  // v2 accepts vault GFM verbatim. Nothing to rewrite.
  return src;
}
