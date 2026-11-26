//! Message text as typed Markdown blocks for the admin page (REQ-019). The
//! page builds elements from these with `textContent` only, so message text
//! never becomes markup: raw HTML stays text and only http, https and mailto
//! links keep their target.
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};
use serde::Serialize;
use std::iter::Peekable;

/// Containers nested deeper than this are flattened to plain text, so a
/// hostile message cannot exhaust the stack here or in the page.
pub const MAX_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Paragraph {
        content: Vec<Inline>,
    },
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    Code {
        language: Option<String>,
        text: String,
    },
    List {
        /// First number of an ordered list; `None` for bullets.
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Quote {
        blocks: Vec<Block>,
    },
    Table {
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Inline {
    Text {
        text: String,
    },
    Code {
        text: String,
    },
    Strong {
        content: Vec<Inline>,
    },
    Emphasis {
        content: Vec<Inline>,
    },
    Strike {
        content: Vec<Inline>,
    },
    /// `href` is `None` when the target is not http, https or mailto.
    Link {
        href: Option<String>,
        content: Vec<Inline>,
    },
    Break,
}

type Events<'a> = Peekable<Parser<'a>>;

/// Parses CommonMark with tables and strikethrough.
pub fn parse(text: &str) -> Vec<Block> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    blocks(&mut Parser::new_ext(text, options).peekable(), 0)
}

/// `href` when it is an http, https or mailto URL.
pub fn safe_href(href: &str) -> Option<String> {
    let scheme = href.split_once(':')?.0.to_ascii_lowercase();
    matches!(scheme.as_str(), "http" | "https" | "mailto").then(|| href.to_owned())
}

fn is_inline(event: &Event) -> bool {
    match event {
        Event::Text(_)
        | Event::Code(_)
        | Event::Html(_)
        | Event::InlineHtml(_)
        | Event::InlineMath(_)
        | Event::DisplayMath(_)
        | Event::FootnoteReference(_)
        | Event::SoftBreak
        | Event::HardBreak
        | Event::TaskListMarker(_) => true,
        Event::Start(tag) => matches!(
            tag,
            Tag::Emphasis
                | Tag::Strong
                | Tag::Strikethrough
                | Tag::Superscript
                | Tag::Subscript
                | Tag::Link { .. }
                | Tag::Image { .. }
        ),
        Event::End(_) | Event::Rule => false,
    }
}

/// Blocks up to the end of the enclosing container (consumed) or the input.
fn blocks(events: &mut Events, depth: usize) -> Vec<Block> {
    let mut out = Vec::new();
    while let Some(event) = events.peek() {
        if matches!(event, Event::End(_)) {
            events.next();
            break;
        }
        if is_inline(event) {
            let mut content = Vec::new();
            while events.peek().is_some_and(is_inline) {
                if let Some(event) = events.next() {
                    content.extend(inline(event, events, depth));
                }
            }
            out.push(Block::Paragraph {
                content: merge(content),
            });
            continue;
        }
        match events.next() {
            Some(Event::Start(_)) if depth >= MAX_DEPTH => out.push(Block::Paragraph {
                content: vec![Inline::Text {
                    text: flatten(events),
                }],
            }),
            Some(Event::Start(tag)) => out.extend(block(tag, events, depth + 1)),
            Some(Event::Rule) => out.push(Block::Rule),
            _ => {}
        }
    }
    out
}

fn block(tag: Tag, events: &mut Events, depth: usize) -> Vec<Block> {
    match tag {
        Tag::Paragraph => vec![Block::Paragraph {
            content: inlines(events, depth),
        }],
        Tag::Heading { level, .. } => vec![Block::Heading {
            level: level as u8,
            content: inlines(events, depth),
        }],
        Tag::CodeBlock(kind) => vec![Block::Code {
            language: match kind {
                CodeBlockKind::Fenced(info) => info.split_whitespace().next().map(str::to_owned),
                CodeBlockKind::Indented => None,
            },
            text: raw(events),
        }],
        Tag::HtmlBlock => vec![Block::Code {
            language: Some("html".into()),
            text: raw(events),
        }],
        Tag::BlockQuote(_) => vec![Block::Quote {
            blocks: blocks(events, depth),
        }],
        Tag::List(start) => {
            let mut items = Vec::new();
            while let Some(event) = events.next() {
                match event {
                    Event::Start(Tag::Item) => items.push(blocks(events, depth)),
                    Event::End(_) => break,
                    _ => {}
                }
            }
            vec![Block::List { start, items }]
        }
        Tag::Table(_) => {
            let (mut head, mut rows) = (Vec::new(), Vec::new());
            while let Some(event) = events.next() {
                match event {
                    Event::Start(Tag::TableHead) => head = cells(events, depth),
                    Event::Start(Tag::TableRow) => rows.push(cells(events, depth)),
                    Event::End(_) => break,
                    _ => {}
                }
            }
            vec![Block::Table { head, rows }]
        }
        // Containers without a type of their own (footnotes, definitions) keep their content.
        _ => blocks(events, depth),
    }
}

fn cells(events: &mut Events, depth: usize) -> Vec<Vec<Inline>> {
    let mut cells = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::Start(Tag::TableCell) => cells.push(inlines(events, depth)),
            Event::End(_) => break,
            _ => {}
        }
    }
    cells
}

/// Inline content up to the end of the enclosing element (consumed).
fn inlines(events: &mut Events, depth: usize) -> Vec<Inline> {
    let mut content = Vec::new();
    while let Some(event) = events.next() {
        if matches!(event, Event::End(_)) {
            break;
        }
        content.extend(inline(event, events, depth));
    }
    merge(content)
}

fn inline(event: Event, events: &mut Events, depth: usize) -> Vec<Inline> {
    let text = |text: &str| vec![Inline::Text { text: text.into() }];
    match event {
        Event::Text(value) | Event::Html(value) | Event::InlineHtml(value) => text(&value),
        Event::Code(value) | Event::InlineMath(value) | Event::DisplayMath(value) => {
            vec![Inline::Code {
                text: value.to_string(),
            }]
        }
        Event::FootnoteReference(label) => text(&format!("[^{label}]")),
        Event::TaskListMarker(done) => text(if done { "[x] " } else { "[ ] " }),
        Event::SoftBreak | Event::HardBreak => vec![Inline::Break],
        Event::Rule => text("---"),
        Event::End(_) => Vec::new(),
        Event::Start(_) if depth >= MAX_DEPTH => text(&flatten(events)),
        Event::Start(tag) => {
            let depth = depth + 1;
            match tag {
                Tag::Strong => vec![Inline::Strong {
                    content: inlines(events, depth),
                }],
                Tag::Emphasis => vec![Inline::Emphasis {
                    content: inlines(events, depth),
                }],
                Tag::Strikethrough => vec![Inline::Strike {
                    content: inlines(events, depth),
                }],
                Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => vec![Inline::Link {
                    href: safe_href(&dest_url),
                    content: inlines(events, depth),
                }],
                _ => inlines(events, depth),
            }
        }
    }
}

/// The text of an element, up to its end (consumed), without recursion.
fn flatten(events: &mut Events) -> String {
    let (mut open, mut out) = (1usize, String::new());
    for event in events.by_ref() {
        match event {
            Event::Start(_) => open += 1,
            Event::End(_) => {
                open -= 1;
                if open == 0 {
                    break;
                }
            }
            Event::Text(value)
            | Event::Code(value)
            | Event::Html(value)
            | Event::InlineHtml(value)
            | Event::InlineMath(value)
            | Event::DisplayMath(value) => out.push_str(&value),
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            _ => {}
        }
    }
    out.trim().to_owned()
}

/// Literal text of a code or HTML block, up to its end (consumed).
fn raw(events: &mut Events) -> String {
    let mut out = String::new();
    for event in events.by_ref() {
        match event {
            Event::End(_) => break,
            Event::Text(value) | Event::Html(value) => out.push_str(&value),
            _ => {}
        }
    }
    out.trim_end_matches('\n').to_owned()
}

/// Joins adjacent text runs, which the parser splits at special characters.
fn merge(content: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::with_capacity(content.len());
    for item in content {
        match (out.last_mut(), item) {
            (Some(Inline::Text { text }), Inline::Text { text: next }) => text.push_str(&next),
            (_, item) => out.push(item),
        }
    }
    out
}

#[cfg(test)]
#[path = "markdown_tests.rs"]
mod tests;
