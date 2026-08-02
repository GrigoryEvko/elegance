//! Vue single-file components: the script inside the markup.
//!
//! A `.vue` file is a container, not a language. Its `<script>` blocks
//! are ordinary TypeScript or JavaScript, and the whole rest of the
//! file — template, styles — is not something this tool has anything
//! true to say about.
//!
//! No new grammar. The community tree-sitter-vue is stale, and a
//! container needs no parser: replacing everything outside the script
//! blocks with BLANK LINES yields a source the TS/JS pack reads
//! directly, at line numbers that are already correct in the real
//! file. `--explain`, `--diff` and the baseline all work unchanged
//! because nothing downstream ever learns an offset existed.
//!
//! Template expressions (`:prop="expr"`, `@click="handler()"`) are out
//! of scope in this tier and stated so rather than silently missed.

use crate::lang::Lang;

/// A `.vue` file's script content as the TS/JS pack should see it,
/// with everything else blanked, plus which pack that is. `None` when
/// the file declares no script at all (a template-only component).
pub fn script_of(source: &str) -> Option<(Lang, String)> {
    let blocks = script_blocks(source);
    if blocks.is_empty() {
        return None;
    }
    // `lang="ts"` on ANY block decides the pack: a component mixing a
    // typed setup block with an untyped options block is still
    // TypeScript, and reading it as JavaScript would silence every
    // type-hygiene metric it earned.
    let lang = match blocks.iter().any(|b| b.typed) {
        true => Lang::TypeScript,
        false => Lang::JavaScript,
    };
    let mut out = String::with_capacity(source.len());
    let mut at = 0;
    for block in &blocks {
        blank_out(&source[at..block.start], &mut out);
        out.push_str(&source[block.start..block.end]);
        at = block.end;
    }
    blank_out(&source[at..], &mut out);
    Some((lang, out))
}

/// Replace a span with the newlines it contained, so every following
/// line keeps its true number and no offset needs tracking.
fn blank_out(span: &str, out: &mut String) {
    for _ in span.bytes().filter(|b| *b == b'\n') {
        out.push('\n');
    }
}

struct Block {
    start: usize,
    end: usize,
    typed: bool,
}

/// Every `<script ...> ... </script>` body in the file. Byte offsets,
/// so multi-byte template text cannot shift them.
fn script_blocks(source: &str) -> Vec<Block> {
    const OPEN: &str = "<script";
    let mut blocks = Vec::new();
    let mut at = 0;
    while let Some(open) = source[at..].find(OPEN) {
        let tag_start = at + open;
        let Some(tag_end) = source[tag_start..].find('>').map(|i| tag_start + i + 1) else {
            break;
        };
        at = tag_end;
        // `<scripts>` or `<scriptFoo` is not a script tag.
        let is_tag = source[tag_start + OPEN.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_whitespace() || c == '>');
        if !is_tag {
            continue;
        }
        let attrs = &source[tag_start..tag_end];
        let Some(close) = source[tag_end..].find("</script").map(|i| tag_end + i) else {
            break;
        };
        blocks.push(Block {
            start: tag_end,
            end: close,
            typed: attrs.contains("lang=\"ts\"") || attrs.contains("lang='ts'"),
        });
        at = close;
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    const SFC: &str = "<template>\n  <div :class=\"cls\">{{ msg }}</div>\n</template>\n\n<script setup lang=\"ts\">\nconst cls = 'x';\nfunction greet(name: string): string {\n  return name;\n}\n</script>\n\n<style scoped>\n.x { color: red; }\n</style>\n";

    #[test]
    fn line_numbers_survive_the_container() {
        let (lang, script) = script_of(SFC).expect("has a script");
        assert_eq!(lang, Lang::TypeScript, "lang=\"ts\" chooses the pack");
        // `function greet` is on line 7 of the real file; it must be on
        // line 7 of what the parser sees, with no offset bookkeeping.
        let at = script
            .lines()
            .position(|l| l.contains("function greet"))
            .expect("script kept");
        assert_eq!(at + 1, 7);
        // The template and styles are gone, not merely ignored.
        assert!(!script.contains("<template>"));
        assert!(!script.contains("color: red"));
        // Same line count in and out: nothing downstream can drift.
        assert_eq!(script.lines().count(), SFC.lines().count());
    }

    #[test]
    fn the_extractor_reads_a_component_as_its_language() {
        let (lang, script) = script_of(SFC).unwrap();
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        let f = crate::facts::extract(
            pack,
            &mut parser,
            std::path::Path::new("Widget.vue"),
            &script,
        );
        assert!(!f.low_confidence(), "a blanked container still parses");
        let greet = f.units.iter().find(|u| &*u.name == "greet").expect("unit");
        assert_eq!(greet.line, 7, "the unit reports its true file line");
        assert_eq!(greet.params.len(), 1);
        assert!(greet.params[0].typed, "TypeScript, not JavaScript");
    }

    #[test]
    fn components_without_script_and_lookalike_tags() {
        assert!(
            script_of("<template>\n  <p>static</p>\n</template>\n").is_none(),
            "a template-only component has nothing to measure"
        );
        // An untyped options block is JavaScript.
        let (lang, _) = script_of("<script>\nexport default {};\n</script>\n").unwrap();
        assert_eq!(lang, Lang::JavaScript);
        // Two blocks: setup plus options, and `lang=ts` on either wins.
        let both = "<script lang=\"ts\">\nconst a: number = 1;\n</script>\n<script setup>\nconst b = 2;\n</script>\n";
        let (lang, script) = script_of(both).unwrap();
        assert_eq!(lang, Lang::TypeScript);
        assert!(script.contains("const a") && script.contains("const b"));
    }
}
