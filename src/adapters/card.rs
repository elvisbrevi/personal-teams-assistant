//! The assistant's messages as an Adaptive Card, when `[message_style]` is on: the answer's
//! Markdown framed by a colored stripe along the left edge and a band with a label above or
//! below it. Teams drops styles from chat HTML, so a card is the only way to color a message.
//!
//! The stripe is a tiny PNG painted down the card's left edge, so any color works; the band
//! uses a Teams container style, which Teams shades for its light or dark theme. The card holds
//! only text and links checked by the pipeline: no actions, inputs or remote images.
use super::teams;
use crate::config::{BandColor, BandPosition, LabelColor, Language, MessageStyle, TextSize};
use base64::Engine;
use serde_json::{Value, json};

/// Graph's attachment type for an Adaptive Card.
pub const CONTENT_TYPE: &str = "application/vnd.microsoft.card.adaptive";
/// The band's element ID: the label is presentation, so reading a card back skips it.
const BAND: &str = "pta-band";
/// Stands for `](` in a link this module wrote; every other `](` is broken up so the card's
/// Markdown never turns text into a link the pipeline did not check.
const LINK: char = '\u{e002}';
/// Graph's limit for a message: the HTML body plus the serialized card.
const LIMIT: usize = 27_800;

/// The Graph message that sends `markdown` as a styled card, with `marker` (the output marker
/// of the personal chat, or nothing) after it. `None` when the style is off or the card would
/// not fit in a message: the caller then sends plain HTML, which always fits a valid answer.
pub fn message(
    markdown: &str,
    style: &MessageStyle,
    language: Language,
    marker: &str,
) -> Option<Value> {
    if !style.enabled {
        return None;
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    let content = serde_json::to_string(&card(markdown, style, language)).ok()?;
    let body = format!("<attachment id=\"{id}\"></attachment>{marker}");
    (body.len() + content.len() <= LIMIT).then(|| {
        json!({
            "body": {"contentType": "html", "content": body},
            "attachments": [{"id": id, "contentType": CONTENT_TYPE, "content": content}],
        })
    })
}

/// The Adaptive Card for `markdown` in `style`.
pub fn card(markdown: &str, style: &MessageStyle, language: Language) -> Value {
    let stripe = json!({
        "url": stripe(&style.accent, style.stripe_width),
        "fillMode": "RepeatVertically",
        "horizontalAlignment": "Left",
    });
    let mut body = elements(markdown, true);
    if style.text_size != TextSize::Default {
        body.iter_mut().for_each(|e| resize(e, style.text_size));
    }
    let mut band = band(style, language, &stripe);
    match style.position {
        BandPosition::Top => {
            if let Some(first) = body.first_mut() {
                first["spacing"] = json!("Medium");
            }
            body.insert(0, band);
        }
        BandPosition::Bottom => {
            if !body.is_empty() {
                band["spacing"] = json!("Medium");
            }
            body.push(band);
        }
    }
    json!({
        "type": "AdaptiveCard",
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "version": "1.5",
        "backgroundImage": stripe,
        "body": body,
    })
}

/// The text of a Graph chat message: its body and the text of its Adaptive Cards, without the
/// band of the assistant's own cards.
pub fn message_text(message: &Value) -> String {
    let body = &message["body"];
    let raw = body["content"].as_str().unwrap_or("");
    let mut text = if body["contentType"].as_str() == Some("html") {
        teams::plain_text(raw)
    } else {
        raw.to_owned()
    };
    for attachment in message["attachments"].as_array().into_iter().flatten() {
        if attachment["contentType"].as_str() != Some(CONTENT_TYPE) {
            continue;
        }
        let Some(card) = attachment["content"]
            .as_str()
            .and_then(|c| serde_json::from_str::<Value>(c).ok())
        else {
            continue;
        };
        let mut parts = Vec::new();
        texts(&card, &mut parts);
        if !parts.is_empty() {
            text.push(' ');
            text.push_str(&parts.join(" "));
        }
    }
    text.replace('\u{200b}', "")
}
fn texts(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if object.get("id").and_then(Value::as_str) == Some(BAND) {
                return;
            }
            for key in ["text", "codeSnippet"] {
                if let Some(text) = object.get(key).and_then(Value::as_str) {
                    out.push(text.to_owned());
                }
            }
            for (key, child) in object {
                // A fallback repeats what it stands for.
                if key != "fallback" {
                    texts(child, out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| texts(item, out)),
        _ => {}
    }
}

fn band(style: &MessageStyle, language: Language, stripe: &Value) -> Value {
    let color = match style.label_color {
        LabelColor::Default | LabelColor::Subtle => "Default",
        LabelColor::Accent => "Accent",
        LabelColor::Good => "Good",
        LabelColor::Warning => "Warning",
        LabelColor::Attention => "Attention",
    };
    let subtle = style.label_color == LabelColor::Subtle;
    let size = style.label_size.card();
    let text = |text: &str| {
        json!({"type": "TextBlock", "text": inline(text), "size": size,
        "wrap": true, "color": color, "isSubtle": subtle})
    };
    let column = |item: Value| json!({"type": "Column", "width": "auto", "verticalContentAlignment": "Center", "items": [item]});
    let mut columns = Vec::new();
    if style.icon {
        // Plain text: Teams does not draw the card `Icon` element in chat messages.
        columns.push(column(text(style.emoji())));
    }
    columns.push(column(text(&style.label(language))));
    let mut band = json!({"type": "Container", "id": BAND,
        "items": [{"type": "ColumnSet", "horizontalAlignment": "Right", "columns": columns}]});
    if style.band_height > 0 {
        band["minHeight"] = json!(format!("{}px", style.band_height));
        band["verticalContentAlignment"] = json!("Center");
    }
    let background = match style.band {
        BandColor::Accent => "accent",
        BandColor::Emphasis => "emphasis",
        BandColor::Good => "good",
        BandColor::Warning => "warning",
        BandColor::Attention => "attention",
        BandColor::None => return band,
    };
    // Bleeding to the card's edges hides the card's stripe, so the band paints it again.
    band["style"] = json!(background);
    band["bleed"] = json!(true);
    band["backgroundImage"] = stripe.clone();
    band
}

/// The answer's Markdown as card elements, with the blocks `teams::html` knows: every line of a
/// paragraph or list is its own text block, so line breaks never depend on the client.
fn elements(markdown: &str, quotes: bool) -> Vec<Value> {
    let item = regex::Regex::new(r"^(\s*)([-*+]|\d{1,3}[.)])\s+(.*)$").unwrap();
    let lines: Vec<&str> = markdown.lines().collect();
    let mut out = Vec::new();
    let mut gap = Gap::Block;
    // Indents of the open lists; empty outside a list.
    let mut lists: Vec<usize> = Vec::new();
    let mut next = 0;
    while let Some(line) = lines.get(next) {
        next += 1;
        let indent = line.chars().take_while(|c| c.is_whitespace()).count();
        let trimmed = line.trim_start();
        if let Some(lang) = trimmed.strip_prefix("```") {
            let mut body = Vec::new();
            while let Some(line) = lines.get(next) {
                next += 1;
                if line.trim_start().starts_with("```") {
                    break;
                }
                let strip = line
                    .chars()
                    .take(indent)
                    .take_while(|c| c.is_whitespace())
                    .count();
                body.push(line.chars().skip(strip).collect::<String>());
            }
            push(&mut out, &mut gap, code(&lang.trim().to_lowercase(), &body));
            gap = Gap::Block;
            lists.clear();
            continue;
        }
        if trimmed.is_empty() {
            if gap == Gap::Line {
                gap = Gap::Block;
            }
            lists.clear();
            continue;
        }
        if indent == 0 && teams::rule(trimmed) {
            gap = Gap::Rule;
            lists.clear();
            continue;
        }
        if indent == 0
            && let Some((level, text)) = teams::heading(trimmed)
        {
            let size = if level <= 2 { "Medium" } else { "Default" };
            push(
                &mut out,
                &mut gap,
                json!({"type": "TextBlock", "text": inline(text),
                "weight": "Bolder", "size": size, "wrap": true}),
            );
            gap = Gap::Block;
            lists.clear();
            continue;
        }
        if quotes && let Some(first) = trimmed.strip_prefix('>') {
            let mut quoted = vec![first];
            while let Some(more) = lines
                .get(next)
                .and_then(|l| l.trim_start().strip_prefix('>'))
            {
                quoted.push(more);
                next += 1;
            }
            let body: Vec<&str> = quoted
                .iter()
                .map(|l| l.strip_prefix(' ').unwrap_or(l))
                .collect();
            let items = elements(&body.join("\n"), false);
            if !items.is_empty() {
                push(
                    &mut out,
                    &mut gap,
                    json!({"type": "Container", "style": "emphasis", "items": items}),
                );
            }
            gap = Gap::Block;
            lists.clear();
            continue;
        }
        let header = trimmed
            .contains('|')
            .then(|| teams::cells(trimmed))
            .filter(|header| {
                lines.get(next).and_then(|l| teams::delimiter(l)) == Some(header.len())
            });
        if let Some(header) = header {
            next += 1;
            let mut rows = Vec::new();
            while let Some(row) = lines.get(next).filter(|l| {
                l.contains('|') && !l.trim().is_empty() && !l.trim_start().starts_with("```")
            }) {
                rows.push(teams::cells(row));
                next += 1;
            }
            push(&mut out, &mut gap, table(&header, &rows));
            gap = Gap::Block;
            lists.clear();
            continue;
        }
        if let Some(c) = item.captures(line) {
            let indent = c[1].chars().count();
            while lists.last().is_some_and(|i| *i > indent) {
                lists.pop();
            }
            if lists.last() != Some(&indent) {
                lists.push(indent);
            }
            let depth = lists.len() - 1;
            let mark = if c[2].starts_with(|c: char| c.is_ascii_digit()) {
                c[2].to_owned()
            } else if depth == 0 {
                "•".to_owned()
            } else {
                "◦".to_owned()
            };
            // Teams has no checkboxes in messages: checklist items start with a ballot box.
            let (check, text) = match c[3].split_at_checked(4) {
                Some(("[ ] ", text)) => ("☐ ", text),
                Some(("[x] " | "[X] ", text)) => ("☑ ", text),
                _ => ("", &c[3]),
            };
            let text = format!("{}{mark} {check}{}", pad(depth), inline(text));
            push(&mut out, &mut gap, text_block(text));
            gap = Gap::Line;
            continue;
        }
        // A line right after a list item continues it; anything else is a paragraph line.
        let text = match lists.len() {
            0 => inline(trimmed),
            depth => format!("{}  {}", pad(depth - 1), inline(trimmed)),
        };
        push(&mut out, &mut gap, text_block(text));
        gap = Gap::Line;
    }
    out
}

/// What separates the next element from the previous one.
#[derive(Clone, Copy, PartialEq)]
enum Gap {
    /// The next line of the same paragraph or list.
    Line,
    /// A new block.
    Block,
    /// A `---` rule.
    Rule,
}
fn push(out: &mut Vec<Value>, gap: &mut Gap, mut element: Value) {
    if !out.is_empty() {
        match gap {
            Gap::Line => element["spacing"] = json!("None"),
            Gap::Block => {}
            Gap::Rule => {
                element["spacing"] = json!("Medium");
                element["separator"] = json!(true);
            }
        }
    }
    *gap = Gap::Block;
    out.push(element);
}
fn text_block(text: String) -> Value {
    json!({"type": "TextBlock", "text": text, "wrap": true})
}
/// Indentation for a nested list: non-breaking spaces, which Markdown keeps.
fn pad(depth: usize) -> String {
    "\u{a0}".repeat(4 * depth)
}

/// A code block. Teams has its own `CodeBlock`; other clients get the lines in a monospace
/// font, as plain text runs that no Markdown touches.
fn code(lang: &str, lines: &[String]) -> Value {
    let language = match lang {
        "sh" | "shell" | "bash" | "curl" => "Bash",
        "cs" | "c#" | "csharp" => "C#",
        "css" => "CSS",
        "go" => "Go",
        "graphql" => "GraphQL",
        "html" => "HTML",
        "java" => "Java",
        "js" | "javascript" => "JavaScript",
        "json" => "JSON",
        "php" => "PHP",
        "ps1" | "powershell" => "PowerShell",
        "py" | "python" => "Python",
        "sql" => "SQL",
        "ts" | "typescript" => "TypeScript",
        "xml" => "XML",
        _ => "PlainText",
    };
    let runs: Vec<Value> = lines
        .iter()
        .map(|line| {
            // An empty run would collapse the blank line.
            let text = if line.is_empty() { "\u{a0}" } else { line };
            json!({"type": "RichTextBlock", "spacing": "None",
                "inlines": [{"type": "TextRun", "text": text, "fontType": "Monospace"}]})
        })
        .collect();
    json!({"type": "CodeBlock", "codeSnippet": lines.join("\n"), "language": language,
        "fallback": {"type": "Container", "style": "emphasis", "items": runs}})
}

fn table(header: &[String], rows: &[Vec<String>]) -> Value {
    let row = |cells: &[String], bold: bool| {
        let cells: Vec<Value> = (0..header.len())
            .map(|k| {
                let mut text = text_block(inline(cells.get(k).map_or("", String::as_str)));
                if bold {
                    text["weight"] = json!("Bolder");
                }
                json!({"type": "TableCell", "items": [text]})
            })
            .collect();
        json!({"type": "TableRow", "cells": cells})
    };
    let mut all = vec![row(header, true)];
    all.extend(rows.iter().map(|cells| row(cells, false)));
    json!({"type": "Table", "firstRowAsHeader": true, "showGridLines": true,
        "columns": vec![json!({"width": 1}); header.len()], "rows": all})
}

/// The answer's inline Markdown in the subset cards render: bold, italic and HTTPS links stay;
/// a color, strikethrough or code span keeps its text. A `](` that is not a checked link is
/// broken with a zero-width space.
fn inline(text: &str) -> String {
    let text = text.replace(LINK, "");
    let mut out = String::new();
    spans(&text, &mut out);
    out.replace("](", "]\u{200b}(").replace(LINK, "](")
}
fn spans(text: &str, out: &mut String) {
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let used = match c {
            '\\' => rest[1..]
                .chars()
                .next()
                .filter(char::is_ascii_punctuation)
                .map(|p| {
                    out.push('\\');
                    out.push(p);
                    2
                }),
            '`' => rest[1..].find('`').map(|end| {
                out.push_str(&rest[1..1 + end]);
                end + 2
            }),
            '~' if rest.starts_with("~~") => {
                rest[2..].find("~~").filter(|end| *end > 0).map(|end| {
                    spans(&rest[2..2 + end], out);
                    end + 4
                })
            }
            '{' => teams::colored(rest).map(|(_, body, used)| {
                spans(body, out);
                used
            }),
            '[' => teams::link(rest).map(|(label, url, used)| {
                out.push('[');
                spans(label, out);
                out.push(LINK);
                out.push_str(url);
                out.push(')');
                used
            }),
            _ => None,
        };
        let used = used.unwrap_or_else(|| {
            out.push(c);
            c.len_utf8()
        });
        rest = &rest[used..];
    }
}

/// Text sizes in the message's elements: headings (the blocks marked `Medium`) one step above
/// `size`, everything else at `size`.
fn resize(element: &mut Value, size: TextSize) {
    match element {
        Value::Object(object) => {
            if matches!(
                object.get("type").and_then(Value::as_str),
                Some("TextBlock" | "TextRun")
            ) {
                let heading = object.get("size").and_then(Value::as_str) == Some("Medium");
                let size = if heading { size.larger() } else { size };
                object.insert("size".into(), json!(size.card()));
            }
            object.values_mut().for_each(|child| resize(child, size));
        }
        Value::Array(items) => items.iter_mut().for_each(|item| resize(item, size)),
        _ => {}
    }
}

/// A data URI for a PNG one pixel tall whose left `width` pixels are `hex`, followed by as many
/// transparent ones: repeated down the card's left edge, it draws a stripe `width` pixels wide.
fn stripe(hex: &str, width: u8) -> String {
    let rgb = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0x5B5FC7);
    let [_, r, g, b] = rgb.to_be_bytes();
    let width = u32::from(width.clamp(2, 16));
    let mut row = vec![0u8]; // filter: none
    for x in 0..2 * width {
        row.extend(if x < width { [r, g, b, 255] } else { [0; 4] });
    }
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend((2 * width).to_be_bytes());
    header.extend(1u32.to_be_bytes());
    header.extend([8, 6, 0, 0, 0]); // 8-bit RGBA
    chunk(&mut png, b"IHDR", &header);
    // zlib with one stored deflate block.
    let mut data = vec![0x78, 0x01, 0x01];
    let len = row.len() as u16;
    data.extend(len.to_le_bytes());
    data.extend((!len).to_le_bytes());
    data.extend(&row);
    data.extend(adler32(&row).to_be_bytes());
    chunk(&mut png, b"IDAT", &data);
    chunk(&mut png, b"IEND", &[]);
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    )
}
fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend((data.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend(kind);
    png.extend(data);
    let crc = crc32(&png[start..]);
    png.extend(crc.to_be_bytes());
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for byte in bytes {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> MessageStyle {
        MessageStyle {
            enabled: true,
            ..MessageStyle::default()
        }
    }
    fn body_texts(card: &Value) -> Vec<String> {
        card["body"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["id"] != BAND)
            .map(|e| e["text"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    #[test]
    fn the_band_follows_the_position_label_and_colors() {
        let card = card("Hola", &style(), Language::Es);
        let body = card["body"].as_array().unwrap();
        assert_eq!(body[0]["text"], "Hola");
        let band = &body[1];
        assert_eq!(band["id"], BAND);
        assert_eq!(band["style"], "accent");
        assert_eq!(band["bleed"], true);
        assert_eq!(band["spacing"], "Medium");
        let columns = band["items"][0]["columns"].as_array().unwrap();
        assert_eq!(columns[0]["items"][0]["type"], "TextBlock");
        assert_eq!(columns[0]["items"][0]["text"], "🤖");
        assert_eq!(columns[1]["items"][0]["text"], "Respuesta generada con IA");
        // The band fits its text unless it has a height.
        assert!(band.get("minHeight").is_none());
        assert_eq!(
            card["backgroundImage"]["url"],
            band["backgroundImage"]["url"]
        );

        let custom = MessageStyle {
            label: "Escrito por mi asistente".into(),
            position: BandPosition::Top,
            band: BandColor::None,
            label_color: LabelColor::Subtle,
            icon: false,
            ..style()
        };
        let card = super::card("Hola", &custom, Language::En);
        let body = card["body"].as_array().unwrap();
        let band = &body[0];
        assert_eq!(band["id"], BAND);
        assert!(band.get("style").is_none() && band.get("bleed").is_none());
        let columns = band["items"][0]["columns"].as_array().unwrap();
        assert_eq!(columns.len(), 1);
        assert_eq!(columns[0]["items"][0]["text"], "Escrito por mi asistente");
        assert_eq!(columns[0]["items"][0]["isSubtle"], true);
        assert_eq!(body[1]["spacing"], "Medium");
        // Any emoji, and a taller band with its text centered.
        let tall = MessageStyle {
            emoji: "✨".into(),
            band_height: 48,
            ..style()
        };
        let card = super::card("Hola", &tall, Language::Es);
        let band = &card["body"][1];
        assert_eq!(band["minHeight"], "48px");
        assert_eq!(band["verticalContentAlignment"], "Center");
        assert_eq!(band["items"][0]["columns"][0]["items"][0]["text"], "✨");
        // The default label follows the reply language.
        let english = MessageStyle {
            position: BandPosition::Top,
            ..style()
        };
        let card = super::card("Hi", &english, Language::En);
        assert_eq!(
            card["body"][0]["items"][0]["columns"][1]["items"][0]["text"],
            "AI-generated response"
        );
    }

    #[test]
    fn the_stripe_is_a_valid_png_in_the_chosen_color() {
        let url = stripe("#5B5FC7", 4);
        let png = base64::engine::general_purpose::STANDARD
            .decode(url.strip_prefix("data:image/png;base64,").unwrap())
            .unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        // Each chunk's CRC covers its type and data.
        let mut at = 8;
        let mut kinds = Vec::new();
        while at < png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let crc = u32::from_be_bytes(png[at + 8 + len..at + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32(&png[at + 4..at + 8 + len]));
            kinds.push(String::from_utf8(png[at + 4..at + 8].to_vec()).unwrap());
            at += 12 + len;
        }
        assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
        // The stored row: filter byte, four colored pixels, four transparent ones.
        let row = &png[8 + 12 + 13 + 8 + 7..][..33];
        assert_eq!(&row[..5], &[0, 0x5B, 0x5F, 0xC7, 255]);
        assert_eq!(&row[17..], &[0; 16]);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        // A wider stripe: twelve colored pixels, then twelve transparent ones.
        let png = base64::engine::general_purpose::STANDARD
            .decode(
                stripe("#13A10E", 12)
                    .strip_prefix("data:image/png;base64,")
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 24);
        let row = &png[8 + 12 + 13 + 8 + 7..][..97];
        assert_eq!(&row[45..49], &[0x13, 0xA1, 0x0E, 255]);
        assert_eq!(&row[49..], &[0; 48]);
    }

    #[test]
    fn markdown_becomes_card_elements() {
        let markdown = "# Pago\nPrimera línea\nsegunda **línea**\n\n- uno\n  - [x] dos\n  sigue\n1. tres\n\n---\n> cita\n\n| A | B |\n| --- | --- |\n| `x` | {green:ok} |\n\n```py\nprint(1)\n\n```";
        let card = card(markdown, &style(), Language::Es);
        let body = card["body"].as_array().unwrap();
        assert_eq!(body[0]["weight"], "Bolder");
        assert_eq!(body[0]["text"], "Pago");
        assert_eq!(body[1]["text"], "Primera línea");
        assert!(body[1].get("spacing").is_none());
        assert_eq!(body[2]["text"], "segunda **línea**");
        assert_eq!(body[2]["spacing"], "None");
        assert_eq!(body[3]["text"], "• uno");
        assert!(body[3].get("spacing").is_none());
        assert_eq!(body[4]["text"], "\u{a0}\u{a0}\u{a0}\u{a0}◦ ☑ dos");
        assert_eq!(body[5]["text"], "\u{a0}\u{a0}\u{a0}\u{a0}  sigue");
        assert_eq!(body[6]["text"], "1. tres");
        assert_eq!(body[7]["type"], "Container");
        assert_eq!(body[7]["separator"], true);
        assert_eq!(body[7]["items"][0]["text"], "cita");
        assert_eq!(body[8]["type"], "Table");
        assert_eq!(body[8]["rows"][0]["cells"][1]["items"][0]["text"], "B");
        assert_eq!(
            body[8]["rows"][0]["cells"][1]["items"][0]["weight"],
            "Bolder"
        );
        assert_eq!(body[8]["rows"][1]["cells"][0]["items"][0]["text"], "x");
        assert_eq!(body[8]["rows"][1]["cells"][1]["items"][0]["text"], "ok");
        assert_eq!(body[9]["type"], "CodeBlock");
        assert_eq!(body[9]["codeSnippet"], "print(1)\n");
        assert_eq!(body[9]["language"], "Python");
        assert_eq!(body[10]["id"], BAND);
        assert_eq!(body.len(), 11);
    }

    #[test]
    fn text_sizes_apply_to_the_message_and_the_band() {
        // By default the message keeps the client's size and the band is small.
        let card = card("# Título\nTexto", &style(), Language::Es);
        assert_eq!(card["body"][0]["size"], "Medium");
        assert!(card["body"][1].get("size").is_none());
        let label = &card["body"][2]["items"][0]["columns"][1]["items"][0];
        assert_eq!(label["size"], "Small");
        let larger = MessageStyle {
            text_size: TextSize::Large,
            label_size: TextSize::Medium,
            ..style()
        };
        let card = super::card(
            "# Título\n### Sub\nTexto\n\n| A |\n| --- |\n| b |\n\n```\nx\n```",
            &larger,
            Language::Es,
        );
        let body = card["body"].as_array().unwrap();
        assert_eq!(body[0]["size"], "ExtraLarge");
        assert_eq!(body[1]["size"], "Large");
        assert_eq!(body[2]["size"], "Large");
        assert_eq!(body[3]["rows"][1]["cells"][0]["items"][0]["size"], "Large");
        assert_eq!(
            body[4]["fallback"]["items"][0]["inlines"][0]["size"],
            "Large"
        );
        let band = body[5]["items"][0]["columns"].as_array().unwrap();
        assert_eq!(band[0]["items"][0]["size"], "Medium");
        assert_eq!(band[1]["items"][0]["size"], "Medium");
    }

    #[test]
    fn only_checked_https_links_stay_links() {
        let card = card(
            "Ver [la página](https://dev.azure.com/o/p/_wiki?x=(1)) y [otra](http://evil.example) o [a](javascript:alert(1)) ~~no~~",
            &style(),
            Language::Es,
        );
        assert_eq!(
            body_texts(&card),
            [
                "Ver [la página](https://dev.azure.com/o/p/_wiki?x=(1)) y [otra]\u{200b}(http://evil.example) o [a]\u{200b}(javascript:alert(1)) no"
            ]
        );
        // A private-use character in the answer cannot forge a link.
        let card = super::card("[a]\u{e002}http://evil.example)", &style(), Language::Es);
        assert_eq!(body_texts(&card), ["[a]http://evil.example)"]);
    }

    #[test]
    fn a_styled_message_attaches_the_card_and_reads_back_without_the_band() {
        assert!(message("Hola", &MessageStyle::default(), Language::Es, "").is_none());
        let marker = "<p><a href=\"https://personalteams.invalid/output/n\">PTA</a></p>";
        let sent = message("Hola **mundo**", &style(), Language::Es, marker).unwrap();
        let id = sent["attachments"][0]["id"].as_str().unwrap();
        assert_eq!(
            sent["body"]["content"],
            format!("<attachment id=\"{id}\"></attachment>{marker}")
        );
        assert_eq!(sent["attachments"][0]["contentType"], CONTENT_TYPE);
        let read = message_text(&sent);
        assert!(read.contains("Hola **mundo**"), "{read}");
        assert!(read.contains("PTA") && !read.contains("generada"), "{read}");
        // A plain message reads as before.
        let plain = json!({"body": {"contentType": "html", "content": "<p>Hola</p>"}});
        assert_eq!(message_text(&plain), "Hola");
        // A card too large for one message falls back to plain HTML.
        let long = "a\n".repeat(1_500);
        assert!(teams::html(&long).len() <= 27_800);
        assert!(message(&long, &style(), Language::Es, "").is_none());
    }
}
