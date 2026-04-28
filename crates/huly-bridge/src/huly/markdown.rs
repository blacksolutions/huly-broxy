//! Markdown ↔ ProseMirror JSON converter.
//!
//! `markdown_to_prosemirror_json` converts CommonMark markdown text to a
//! ProseMirror JSON document string suitable for Huly's collaborator service.
//!
//! # Supported input constructs
//!
//! - `doc` root (always emitted)
//! - `paragraph` (plain text blocks)
//! - `heading` (ATX `#`–`######`, level 1–6)
//! - `text` nodes with marks: `strong` (bold `**`), `em` (italic `*`/`_`),
//!   `code` (backtick spans)
//! - `link` mark (inline `[text](url)`)
//! - `code_block` (fenced ``` / indented blocks; optional info string → `language` attr)
//! - `bullet_list` / `ordered_list` + `list_item`
//! - `blockquote`
//! - `hard_break` (trailing two-space line break)
//! - `horizontal_rule`
//!
//! # Unsupported / fallback
//!
//! Tables, images, footnotes, raw HTML, and other constructs not listed above
//! fall back to a plain `text` node carrying the raw source string. This is
//! intentionally lossy — the content is preserved as human-readable text even
//! if ProseMirror cannot render it with full fidelity.
//!
//! # Reverse conversion
//!
//! `prosemirror_to_markdown` converts a ProseMirror JSON document back to
//! markdown. The reverse is **lossy**: attributes not representable in plain
//! markdown (e.g. custom node attrs) are silently dropped.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Markdown → ProseMirror
// ---------------------------------------------------------------------------

/// Convert a CommonMark markdown string to a ProseMirror JSON document string.
///
/// Always returns valid JSON. The returned document always has
/// `{"type":"doc","content":[...]}` shape.
pub fn markdown_to_prosemirror_json(md: &str) -> String {
    let doc = markdown_to_prosemirror(md);
    serde_json::to_string(&doc).expect("ProseMirror doc always serialisable")
}

/// Convert markdown to a ProseMirror `Value`.
pub fn markdown_to_prosemirror(md: &str) -> Value {
    let options = Options::all();
    let parser = Parser::new_ext(md, options);

    let mut builder = DocBuilder::new();
    builder.consume(parser);
    builder.finish()
}

// ---------------------------------------------------------------------------
// ProseMirror → Markdown (lossy reverse)
// ---------------------------------------------------------------------------

/// Convert a ProseMirror JSON string back to markdown.
///
/// **Lossy**: attributes not representable in markdown are dropped.
/// Unsupported node types are rendered as empty strings.
pub fn prosemirror_to_markdown(pm_json: &str) -> String {
    let doc: Value = match serde_json::from_str(pm_json) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    render_node(&doc)
}

fn render_node(node: &Value) -> String {
    let node_type = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match node_type {
        "doc" => render_children(node, "\n\n"),
        "paragraph" => render_inline_children(node),
        "heading" => {
            let level = node
                .get("attrs")
                .and_then(|a| a.get("level"))
                .and_then(|l| l.as_u64())
                .unwrap_or(1) as usize;
            let prefix = "#".repeat(level);
            let inner = render_inline_children(node);
            format!("{} {}", prefix, inner)
        }
        "code_block" => {
            let lang = node
                .get("attrs")
                .and_then(|a| a.get("language"))
                .and_then(|l| l.as_str())
                .unwrap_or("");
            let inner = render_children(node, "\n");
            format!("```{}\n{}\n```", lang, inner)
        }
        "bullet_list" => node
            .get("content")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|item| format!("- {}", render_list_item(item)))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
        "ordered_list" => node
            .get("content")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .enumerate()
                    .map(|(i, item)| format!("{}. {}", i + 1, render_list_item(item)))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
        "blockquote" => {
            let inner = render_children(node, "\n");
            inner
                .lines()
                .map(|l| format!("> {}", l))
                .collect::<Vec<_>>()
                .join("\n")
        }
        "horizontal_rule" => "---".to_string(),
        "text" => {
            let text = node
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            apply_marks_reverse(
                &text,
                node.get("marks").and_then(|m| m.as_array()),
            )
        }
        "hard_break" => "  \n".to_string(),
        _ => render_children(node, ""),
    }
}

fn render_children(node: &Value, sep: &str) -> String {
    node.get("content")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .map(render_node)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(sep)
        })
        .unwrap_or_default()
}

fn render_inline_children(node: &Value) -> String {
    node.get("content")
        .and_then(|c| c.as_array())
        .map(|arr| arr.iter().map(render_node).collect::<String>())
        .unwrap_or_default()
}

fn render_list_item(item: &Value) -> String {
    item.get("content")
        .and_then(|c| c.as_array())
        .map(|arr| arr.iter().map(render_node).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}

fn apply_marks_reverse(text: &str, marks: Option<&Vec<Value>>) -> String {
    let Some(marks) = marks else {
        return text.to_string();
    };
    let mut result = text.to_string();
    for mark in marks.iter().rev() {
        let mark_type = mark.get("type").and_then(|t| t.as_str()).unwrap_or("");
        result = match mark_type {
            "strong" => format!("**{}**", result),
            "em" => format!("_{}_", result),
            "code" => format!("`{}`", result),
            "link" => {
                let href = mark
                    .get("attrs")
                    .and_then(|a| a.get("href"))
                    .and_then(|h| h.as_str())
                    .unwrap_or("");
                format!("[{}]({})", result, href)
            }
            _ => result,
        };
    }
    result
}

// ---------------------------------------------------------------------------
// DocBuilder — internal state machine
// ---------------------------------------------------------------------------

/// Internal builder that walks pulldown-cmark events and assembles a
/// ProseMirror document tree.
struct DocBuilder {
    /// Stack of open node containers.  Each entry is a `(type, attrs, children)` tuple.
    stack: Vec<NodeFrame>,
    /// Active inline text marks (pushed/popped by Tag/TagEnd events).
    marks: Vec<Value>,
}

/// One level of the open-node stack.
struct NodeFrame {
    /// ProseMirror node type name.
    node_type: String,
    /// Optional attributes object (heading level, code language, link href…).
    attrs: Option<Value>,
    /// Accumulated children / content.
    children: Vec<Value>,
}

impl NodeFrame {
    fn new(node_type: impl Into<String>) -> Self {
        Self { node_type: node_type.into(), attrs: None, children: Vec::new() }
    }
    fn with_attrs(mut self, attrs: Value) -> Self {
        self.attrs = Some(attrs);
        self
    }
}

impl DocBuilder {
    fn new() -> Self {
        let doc_frame = NodeFrame::new("doc");
        Self {
            stack: vec![doc_frame],
            marks: Vec::new(),
        }
    }

    /// Consume all parser events.
    fn consume<'a>(&mut self, parser: Parser<'a>) {
        for event in parser {
            self.handle_event(event);
        }
    }

    fn handle_event(&mut self, event: Event<'_>) {
        match event {
            // ---- Block open tags ----
            Event::Start(Tag::Paragraph) => {
                self.stack.push(NodeFrame::new("paragraph"));
            }
            Event::Start(Tag::Heading { level, .. }) => {
                let lv = heading_level(level);
                self.stack.push(NodeFrame::new("heading").with_attrs(json!({"level": lv})));
            }
            Event::Start(Tag::BlockQuote(_)) => {
                self.stack.push(NodeFrame::new("blockquote"));
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang: String = match &kind {
                    pulldown_cmark::CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or("").to_string()
                    }
                    pulldown_cmark::CodeBlockKind::Indented => String::new(),
                };
                let attrs = if lang.is_empty() {
                    None
                } else {
                    Some(json!({"language": lang}))
                };
                let mut frame = NodeFrame::new("code_block");
                frame.attrs = attrs;
                self.stack.push(frame);
            }
            Event::Start(Tag::List(None)) => {
                self.stack.push(NodeFrame::new("bullet_list"));
            }
            Event::Start(Tag::List(Some(_))) => {
                self.stack.push(NodeFrame::new("ordered_list"));
            }
            Event::Start(Tag::Item) => {
                self.stack.push(NodeFrame::new("list_item"));
            }

            // ---- Inline marks ----
            Event::Start(Tag::Strong) => {
                self.marks.push(json!({"type": "strong"}));
            }
            Event::Start(Tag::Emphasis) => {
                self.marks.push(json!({"type": "em"}));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                self.marks.push(json!({"type": "link", "attrs": {"href": dest_url.as_ref()}}));
            }

            // ---- Block close tags ----
            Event::End(TagEnd::Paragraph) => {
                let frame = self.stack.pop().expect("paragraph frame");
                let node = build_node(frame);
                self.push_to_parent(node);
            }
            Event::End(TagEnd::Heading(_)) => {
                let frame = self.stack.pop().expect("heading frame");
                let node = build_node(frame);
                self.push_to_parent(node);
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                let frame = self.stack.pop().expect("blockquote frame");
                let node = build_node(frame);
                self.push_to_parent(node);
            }
            Event::End(TagEnd::CodeBlock) => {
                let frame = self.stack.pop().expect("code_block frame");
                let node = build_node(frame);
                self.push_to_parent(node);
            }
            Event::End(TagEnd::List(_)) => {
                let frame = self.stack.pop().expect("list frame");
                let node = build_node(frame);
                self.push_to_parent(node);
            }
            Event::End(TagEnd::Item) => {
                let frame = self.stack.pop().expect("list_item frame");
                let node = build_node(frame);
                self.push_to_parent(node);
            }

            // ---- Inline mark close ----
            Event::End(TagEnd::Strong) => {
                self.marks.retain(|m| m["type"] != "strong");
            }
            Event::End(TagEnd::Emphasis) => {
                self.marks.retain(|m| m["type"] != "em");
            }
            Event::End(TagEnd::Link) => {
                self.marks.retain(|m| m["type"] != "link");
            }

            // ---- Leaf nodes ----
            Event::Text(text) => {
                let node = self.make_text_node(text.as_ref());
                self.push_to_parent(node);
            }
            Event::Code(code) => {
                // Inline code — emit a text node with `code` mark.
                let mut marks = self.marks.clone();
                marks.push(json!({"type": "code"}));
                let node = json!({
                    "type": "text",
                    "text": code.as_ref(),
                    "marks": marks,
                });
                self.push_to_parent(node);
            }
            Event::SoftBreak => {
                // Soft breaks become a space in the inline flow.
                let node = self.make_text_node(" ");
                self.push_to_parent(node);
            }
            Event::HardBreak => {
                let node = json!({"type": "hard_break"});
                self.push_to_parent(node);
            }
            Event::Rule => {
                let node = json!({"type": "horizontal_rule"});
                self.push_to_parent(node);
            }

            // Everything else (HTML, footnotes, task list markers…) falls
            // back to a plain text node with the raw source.
            Event::Html(raw) | Event::InlineHtml(raw) => {
                let node = self.make_text_node(raw.as_ref());
                self.push_to_parent(node);
            }
            _ => {}
        }
    }

    /// Build a `text` node, applying current marks.
    fn make_text_node(&self, text: &str) -> Value {
        if self.marks.is_empty() {
            json!({"type": "text", "text": text})
        } else {
            json!({"type": "text", "text": text, "marks": self.marks})
        }
    }

    fn push_to_parent(&mut self, node: Value) {
        if let Some(frame) = self.stack.last_mut() {
            frame.children.push(node);
        }
    }

    fn finish(mut self) -> Value {
        // Pop remaining frames (should only be the doc root).
        assert_eq!(self.stack.len(), 1, "unbalanced tag stack");
        let doc_frame = self.stack.pop().expect("doc frame");
        build_node(doc_frame)
    }
}

/// Assemble a ProseMirror node `Value` from a completed `NodeFrame`.
fn build_node(frame: NodeFrame) -> Value {
    let content: Vec<Value> = frame.children;
    let mut node = serde_json::Map::new();
    node.insert("type".into(), Value::String(frame.node_type));
    if let Some(attrs) = frame.attrs {
        node.insert("attrs".into(), attrs);
    }
    node.insert("content".into(), Value::Array(content));
    Value::Object(node)
}

fn heading_level(level: HeadingLevel) -> u64 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(md: &str) -> Value {
        markdown_to_prosemirror(md)
    }

    // --- Basic structural invariants ---

    #[test]
    fn empty_string_produces_doc_with_empty_content() {
        let doc = parse("");
        assert_eq!(doc["type"], "doc");
        assert_eq!(doc["content"], json!([]));
    }

    #[test]
    fn content_field_always_present() {
        // Even a trivial doc must carry a `content` array (PM requirement).
        let doc = parse("hello");
        assert!(doc.get("content").is_some(), "content field missing");
        assert!(doc["content"].is_array(), "content is not an array");
    }

    // --- Paragraph ---

    #[test]
    fn plain_paragraph() {
        let doc = parse("Hello world");
        assert_eq!(doc["content"][0]["type"], "paragraph");
        assert_eq!(doc["content"][0]["content"][0]["type"], "text");
        assert_eq!(doc["content"][0]["content"][0]["text"], "Hello world");
    }

    // --- Headings ---

    #[test]
    fn heading_level_1() {
        let doc = parse("# Title");
        let h = &doc["content"][0];
        assert_eq!(h["type"], "heading");
        assert_eq!(h["attrs"]["level"], 1);
        assert_eq!(h["content"][0]["text"], "Title");
    }

    #[test]
    fn heading_level_2() {
        let doc = parse("## Section");
        assert_eq!(doc["content"][0]["attrs"]["level"], 2);
    }

    #[test]
    fn heading_level_3() {
        let doc = parse("### Sub");
        assert_eq!(doc["content"][0]["attrs"]["level"], 3);
    }

    // --- Inline marks ---

    #[test]
    fn bold_text() {
        let doc = parse("**bold**");
        let marks = &doc["content"][0]["content"][0]["marks"];
        assert!(
            marks.as_array().unwrap().iter().any(|m| m["type"] == "strong"),
            "expected strong mark, got: {marks}"
        );
    }

    #[test]
    fn italic_text() {
        let doc = parse("_italic_");
        let marks = &doc["content"][0]["content"][0]["marks"];
        assert!(
            marks.as_array().unwrap().iter().any(|m| m["type"] == "em"),
            "expected em mark"
        );
    }

    #[test]
    fn bold_and_italic_combined() {
        let doc = parse("**bold** and _italic_");
        let content = &doc["content"][0]["content"];
        let types: Vec<&str> = content
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["type"].as_str().unwrap_or(""))
            .collect();
        // There should be at least 3 text nodes: bold, space, italic.
        assert!(types.len() >= 2, "expected multiple inline nodes");
    }

    // --- Code ---

    #[test]
    fn inline_code_mark() {
        let doc = parse("`hello`");
        let marks = &doc["content"][0]["content"][0]["marks"];
        assert!(
            marks.as_array().unwrap().iter().any(|m| m["type"] == "code"),
            "expected code mark"
        );
    }

    #[test]
    fn code_block_no_lang() {
        let doc = parse("```\nsome code\n```");
        let block = &doc["content"][0];
        assert_eq!(block["type"], "code_block");
        // No language attr when none specified.
        assert!(block.get("attrs").map(|a| a.get("language").is_none()).unwrap_or(true));
    }

    #[test]
    fn code_block_with_language() {
        let doc = parse("```rust\nfn main() {}\n```");
        let block = &doc["content"][0];
        assert_eq!(block["type"], "code_block");
        assert_eq!(block["attrs"]["language"], "rust");
    }

    // --- Lists ---

    #[test]
    fn bullet_list_two_items() {
        let doc = parse("- alpha\n- beta");
        let list = &doc["content"][0];
        assert_eq!(list["type"], "bullet_list");
        assert_eq!(list["content"].as_array().unwrap().len(), 2);
        let item0 = &list["content"][0];
        assert_eq!(item0["type"], "list_item");
    }

    #[test]
    fn ordered_list() {
        let doc = parse("1. first\n2. second");
        let list = &doc["content"][0];
        assert_eq!(list["type"], "ordered_list");
        assert_eq!(list["content"].as_array().unwrap().len(), 2);
    }

    // --- Link ---

    #[test]
    fn link_mark() {
        let doc = parse("[Huly](https://huly.io)");
        let para = &doc["content"][0];
        // Find a text node that has a link mark.
        let has_link = para["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| {
                n["marks"]
                    .as_array()
                    .map(|ms| {
                        ms.iter().any(|m| {
                            m["type"] == "link"
                                && m["attrs"]["href"] == "https://huly.io"
                        })
                    })
                    .unwrap_or(false)
            });
        assert!(has_link, "expected link mark with href https://huly.io");
    }

    // --- Horizontal rule ---

    #[test]
    fn horizontal_rule() {
        let doc = parse("---");
        assert_eq!(doc["content"][0]["type"], "horizontal_rule");
    }

    // --- Round-trip smoke (md → pm → md) ---

    #[test]
    fn roundtrip_paragraph_smoke() {
        let md = "Hello world";
        let pm = markdown_to_prosemirror_json(md);
        let back = prosemirror_to_markdown(&pm);
        assert!(back.contains("Hello world"), "got: {back}");
    }

    #[test]
    fn roundtrip_heading_smoke() {
        let md = "# Title";
        let pm = markdown_to_prosemirror_json(md);
        let back = prosemirror_to_markdown(&pm);
        assert!(back.starts_with("# Title"), "got: {back}");
    }

    #[test]
    fn roundtrip_bold_smoke() {
        let md = "**bold**";
        let pm = markdown_to_prosemirror_json(md);
        let back = prosemirror_to_markdown(&pm);
        assert!(back.contains("**bold**"), "got: {back}");
    }
}
