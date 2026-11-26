use super::*;
use proptest::prelude::*;

fn text(value: &str) -> Inline {
    Inline::Text { text: value.into() }
}
fn paragraph(content: Vec<Inline>) -> Block {
    Block::Paragraph { content }
}
fn depth(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .map(|block| match block {
            Block::Quote { blocks } => 1 + depth(blocks),
            Block::List { items, .. } => 1 + items.iter().map(|i| depth(i)).max().unwrap_or(0),
            _ => 1,
        })
        .max()
        .unwrap_or(0)
}
fn links(blocks: &[Block]) -> Vec<Option<String>> {
    fn inline(items: &[Inline], out: &mut Vec<Option<String>>) {
        for item in items {
            match item {
                Inline::Link { href, content } => {
                    out.push(href.clone());
                    inline(content, out);
                }
                Inline::Strong { content }
                | Inline::Emphasis { content }
                | Inline::Strike { content } => inline(content, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for block in blocks {
        match block {
            Block::Paragraph { content } | Block::Heading { content, .. } => {
                inline(content, &mut out)
            }
            Block::List { items, .. } => {
                for item in items {
                    out.extend(links(item));
                }
            }
            Block::Quote { blocks } => out.extend(links(blocks)),
            Block::Table { head, rows } => {
                for cell in head.iter().chain(rows.iter().flatten()) {
                    inline(cell, &mut out);
                }
            }
            Block::Code { .. } | Block::Rule => {}
        }
    }
    out
}

/* REQ-019: Paragraphs keep the author's line breaks; inline styles become typed spans. */
#[test]
fn parses_paragraphs_and_inline_styles() {
    assert_eq!(
        parse("Found **two** _issues_ in `main.rs` ~~today~~.\nSecond line."),
        [paragraph(vec![
            text("Found "),
            Inline::Strong {
                content: vec![text("two")]
            },
            text(" "),
            Inline::Emphasis {
                content: vec![text("issues")]
            },
            text(" in "),
            Inline::Code {
                text: "main.rs".into()
            },
            text(" "),
            Inline::Strike {
                content: vec![text("today")]
            },
            text("."),
            Inline::Break,
            text("Second line."),
        ])]
    );
    assert_eq!(parse(""), []);
}

/* REQ-019: Headings, fenced code, rules and quotes keep their structure. */
#[test]
fn parses_block_structure() {
    let blocks = parse("## Result\n\n```rust\nfn main() {}\n```\n\n---\n\n> quoted *text*\n");
    assert_eq!(
        blocks,
        [
            Block::Heading {
                level: 2,
                content: vec![text("Result")]
            },
            Block::Code {
                language: Some("rust".into()),
                text: "fn main() {}".into()
            },
            Block::Rule,
            Block::Quote {
                blocks: vec![paragraph(vec![
                    text("quoted "),
                    Inline::Emphasis {
                        content: vec![text("text")]
                    }
                ])]
            },
        ]
    );
    assert_eq!(
        parse("    indented\n"),
        [Block::Code {
            language: None,
            text: "indented".into()
        }]
    );
}

/* REQ-019: Bullet, numbered and nested lists become items of blocks. */
#[test]
fn parses_lists() {
    assert_eq!(
        parse("3. first\n4. second\n   - nested\n"),
        [Block::List {
            start: Some(3),
            items: vec![
                vec![paragraph(vec![text("first")])],
                vec![
                    paragraph(vec![text("second")]),
                    Block::List {
                        start: None,
                        items: vec![vec![paragraph(vec![text("nested")])]]
                    }
                ],
            ]
        }]
    );
    assert_eq!(
        parse("- a\n\n- b\n"),
        [Block::List {
            start: None,
            items: vec![
                vec![paragraph(vec![text("a")])],
                vec![paragraph(vec![text("b")])]
            ]
        }]
    );
}

/* REQ-019: Tables keep their header and cells. */
#[test]
fn parses_tables() {
    assert_eq!(
        parse("| File | State |\n|---|---|\n| a.rs | **ok** |\n"),
        [Block::Table {
            head: vec![vec![text("File")], vec![text("State")]],
            rows: vec![vec![
                vec![text("a.rs")],
                vec![Inline::Strong {
                    content: vec![text("ok")]
                }]
            ]]
        }]
    );
}

/* REQ-019: Raw HTML stays text; only http, https and mailto links keep a target. */
#[test]
fn keeps_html_as_text_and_filters_links() {
    assert_eq!(
        parse("<script>alert(1)</script>\n"),
        [Block::Code {
            language: Some("html".into()),
            text: "<script>alert(1)</script>".into()
        }]
    );
    assert_eq!(
        parse("a <b>bold</b>"),
        [paragraph(vec![text("a <b>bold</b>")])]
    );
    assert_eq!(
        parse("[docs](https://example.com/a) [x](javascript:alert(1)) ![img](http://e.com/i.png)"),
        [paragraph(vec![
            Inline::Link {
                href: Some("https://example.com/a".into()),
                content: vec![text("docs")]
            },
            text(" "),
            Inline::Link {
                href: None,
                content: vec![text("x")]
            },
            text(" "),
            Inline::Link {
                href: Some("http://e.com/i.png".into()),
                content: vec![text("img")]
            },
        ])]
    );
    assert_eq!(
        safe_href("MAILTO:me@example.com").as_deref(),
        Some("MAILTO:me@example.com")
    );
    for bad in [
        "javascript:x",
        "data:text/html,x",
        "/relative",
        "file:///etc",
        "vbscript:x",
        "",
    ] {
        assert_eq!(safe_href(bad), None, "{bad}");
    }
}

/* REQ-019: Deep nesting is flattened to text instead of exhausting the stack. */
#[test]
fn flattens_deep_nesting() {
    let deep = format!("{} bottom", ">".repeat(15_000));
    let blocks = parse(&deep);
    assert!(depth(&blocks) <= MAX_DEPTH + 1, "{}", depth(&blocks));
    let nested_lists: String = (0..400)
        .map(|i| format!("{}- item\n", "  ".repeat(i)))
        .collect();
    assert!(depth(&parse(&nested_lists)) <= MAX_DEPTH + 1);
    let emphasis = format!("{}x{}", "*_".repeat(4000), "_*".repeat(4000));
    parse(&emphasis);
}

proptest! {
    /* REQ-019: Any message parses without panicking, and every kept link target is safe. */
    #[test]
    fn any_text_parses_with_safe_links(input in "(?s).{0,400}") {
        let blocks = parse(&input);
        for href in links(&blocks).into_iter().flatten() {
            prop_assert!(safe_href(&href).is_some(), "{href}");
        }
        prop_assert!(depth(&blocks) <= MAX_DEPTH + 1);
    }

    /* REQ-019: Markdown-like input of any shape parses without panicking. */
    #[test]
    fn markdown_shapes_parse(parts in prop::collection::vec(
        prop::sample::select(vec![
            "# ", "> ", "- ", "1. ", "```", "\n", "\n\n", "**", "_", "`", "~~", "|", "---",
            "[a](", ")", "<b>", "word", "    ", "http://x.y", "javascript:z",
        ]),
        0..80,
    )) {
        let input: String = parts.concat();
        let blocks = parse(&input);
        prop_assert!(depth(&blocks) <= MAX_DEPTH + 1);
    }

    /* REQ-019: Plain words come back as one paragraph with the same text. */
    #[test]
    fn plain_words_round_trip(words in prop::collection::vec("[a-zA-Z0-9]{1,8}", 1..12)) {
        let input = words.join(" ");
        prop_assert_eq!(parse(&input), vec![paragraph(vec![text(&input)])]);
    }
}
