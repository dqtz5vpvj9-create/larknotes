//! Translate LaTeX math between local source form and what survives a
//! Lark roundtrip.
//!
//! ## The problem
//!
//! `$\mathcal{C}_{pre}$ 与 $\mathcal{C}_{post}$` becomes
//! `*{pre} 与 \mathcal{C}*<equation>\mathcal{C}{post}</equation>`
//! after a push/pull cycle. Lark's markdown→docx converter parses
//! markdown italic (`_text_`) before recognising `$...$` math, so two
//! adjacent inline expressions whose underscores can pair across the
//! `$ … $` boundary get their `_` consumed as italic delimiters and the
//! `$` characters fall through as plain text.
//!
//! Wrapping the math in `<equation>` tags before push doesn't help —
//! markdown italic still pairs across HTML tag boundaries.
//!
//! ## The fix (verified by smoke test, 2026-04-20)
//!
//! Escape every `_` inside `$...$` / `$$...$$` to `\_` before push.
//! Lark sees no italic candidates and accepts the math as-is, then
//! exports it as `<equation>...\_{...}...</equation>`. On pull, replace
//! `<equation>` tags with `$...$` and undo `\_` → `_` inside.

use regex::{Captures, Regex};
use std::sync::OnceLock;

fn block_math_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)\$\$(.+?)\$\$").unwrap())
}

fn inline_math_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Pandoc / KaTeX rule: open `$` not followed by whitespace, close `$`
    // not preceded by whitespace. Body forbids `$` so we don't glom across
    // expressions on the same line.
    RE.get_or_init(|| Regex::new(r"\$([^\s$](?:[^$]*[^\s$])?)\$").unwrap())
}

fn equation_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)<equation>(.*?)</equation>").unwrap())
}

/// Escape every `_` inside math regions to `\_`. An already-escaped
/// `\_` (preceded by an odd number of backslashes) is left alone.
fn escape_underscores(body: &str) -> String {
    let mut out = String::with_capacity(body.len() + 4);
    for ch in body.chars() {
        if ch == '_' {
            let bs = out.chars().rev().take_while(|c| *c == '\\').count();
            if bs % 2 == 0 {
                out.push('\\');
            }
        }
        out.push(ch);
    }
    out
}

fn unescape_underscores(body: &str) -> String {
    // Replace `\_` with `_`, leaving other escapes alone.
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'_') {
            out.push('_');
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

/// Pre-push: escape `_` inside every `$...$` / `$$...$$` so Lark's
/// markdown parser doesn't pair them as italic delimiters across math
/// expressions. Block math is processed first to keep its delimiters
/// from being chewed by the inline pass.
pub fn push_math_to_equation(md: &str) -> String {
    let s = block_math_re().replace_all(md, |caps: &Captures| {
        format!("$${}$$", escape_underscores(&caps[1]))
    });
    inline_math_re()
        .replace_all(&s, |caps: &Captures| {
            format!("${}$", escape_underscores(&caps[1]))
        })
        .into_owned()
}

/// Post-pull: convert Lark's `<equation>...</equation>` output back to
/// `$...$` (block/inline distinction is lost — Lark uses one tag for
/// both — and undo the `\_` → `_` introduced on push.
pub fn pull_equation_to_math(md: &str) -> String {
    equation_tag_re()
        .replace_all(md, |caps: &Captures| {
            format!("${}$", unescape_underscores(&caps[1]))
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_escapes_underscore_inside_math() {
        assert_eq!(
            push_math_to_equation(r"a $x_{i}$ b"),
            r"a $x\_{i}$ b"
        );
    }

    #[test]
    fn push_user_bug_case_escapes_both() {
        let input = r"两部分 $\mathcal{C}_{pre}$ 与 $\mathcal{C}_{post}$";
        let expected = r"两部分 $\mathcal{C}\_{pre}$ 与 $\mathcal{C}\_{post}$";
        assert_eq!(push_math_to_equation(input), expected);
    }

    #[test]
    fn push_leaves_underscore_outside_math_alone() {
        // Underscores in prose / identifiers / file names must not be touched.
        let input = "see file_name.md and run my_function()";
        assert_eq!(push_math_to_equation(input), input);
    }

    #[test]
    fn push_does_not_double_escape() {
        // Already-escaped `\_` stays as `\_`, not `\\_`.
        let input = r"$x\_{i}$";
        assert_eq!(push_math_to_equation(input), r"$x\_{i}$");
    }

    #[test]
    fn push_block_math_escapes_underscores() {
        let input = "$$\nx_{i} = y_{j}\n$$";
        assert_eq!(
            push_math_to_equation(input),
            "$$\nx\\_{i} = y\\_{j}\n$$"
        );
    }

    #[test]
    fn push_ignores_currency_with_space() {
        // `$5 and $10` is prose, not math. Strict boundary regex skips it.
        let input = "earned $5 and $10 today";
        assert_eq!(push_math_to_equation(input), input);
    }

    #[test]
    fn pull_unescapes_underscores_inside_equation_tag() {
        assert_eq!(
            pull_equation_to_math(r"a <equation>\mathcal{C}\_{pre}</equation> b"),
            r"a $\mathcal{C}_{pre}$ b"
        );
    }

    #[test]
    fn pull_leaves_other_escapes_alone() {
        // `\\` and `\{` etc. must not be touched.
        assert_eq!(
            pull_equation_to_math(r"<equation>\frac{a}{b} \\ c\_{i}</equation>"),
            r"$\frac{a}{b} \\ c_{i}$"
        );
    }

    #[test]
    fn roundtrip_user_bug_case() {
        let original = r"两部分 $\mathcal{C}_{pre}$ 与 $\mathcal{C}_{post}$";
        let pushed = push_math_to_equation(original);
        // Simulate Lark serializing math back as <equation> tags
        let lark_output = pushed
            .replace(r"$\mathcal{C}\_{pre}$", r"<equation>\mathcal{C}\_{pre}</equation>")
            .replace(r"$\mathcal{C}\_{post}$", r"<equation>\mathcal{C}\_{post}</equation>");
        let pulled = pull_equation_to_math(&lark_output);
        assert_eq!(pulled, original);
    }

    #[test]
    fn roundtrip_realistic_paragraph() {
        let original = r"根据TQ信号 $t_{\text{TQ}}$ 在事件流 $E$ 中的切分相对位置，所有具有历史轨迹的界面动作 $\sigma$ 被绝对二分为两种类型";
        let pushed = push_math_to_equation(original);
        // Simulate Lark output
        let lark_output = pushed
            .replace(r"$t\_{\text{TQ}}$", r"<equation>t\_{\text{TQ}}</equation>")
            .replace("$E$", "<equation>E</equation>")
            .replace(r"$\sigma$", r"<equation>\sigma</equation>");
        let pulled = pull_equation_to_math(&lark_output);
        assert_eq!(pulled, original);
    }
}
