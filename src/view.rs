//! What the WhatsApp screens draw: a message as a phone shows it, a
//! template's category tile and its state.

use laterite_admin::{AdminAsset, Shell};
use laterite_core::{t, Text};

use crate::Preview;

/// The screens' stylesheet, as an admin asset key.
pub(crate) const CSS: &str = "rainmill/whatsapp.css";
/// The template editor's script, as an admin asset key.
pub(crate) const JS: &str = "rainmill/whatsapp.js";

pub(crate) fn css() -> AdminAsset {
    AdminAsset::css(include_bytes!("../assets/whatsapp.css"))
}

pub(crate) fn js() -> AdminAsset {
    AdminAsset::js(include_bytes!("../assets/whatsapp.js"))
}

pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Wraps what sits between a pair of `sign` in `tag`, as WhatsApp reads its
/// marks: the pair is on one line, hugs its text, and stands apart from the
/// words around it.
fn mark(text: &str, sign: char, tag: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let word = |c: char| c.is_alphanumeric() || c == sign;
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let opens = chars[i] == sign
            && (i == 0 || !word(chars[i - 1]))
            && chars
                .get(i + 1)
                .is_some_and(|c| !c.is_whitespace() && *c != sign);
        let close = opens
            .then(|| {
                (i + 2..chars.len())
                    .take_while(|j| chars[*j - 1] != '\n')
                    .find(|j| chars[*j] == sign)
            })
            .flatten()
            .filter(|j| {
                !chars[j - 1].is_whitespace() && chars.get(j + 1).is_none_or(|c| !word(*c))
            });
        match close {
            Some(j) => {
                let inner: String = chars[i + 1..j].iter().collect();
                out.push_str(&format!("<{tag}>{inner}</{tag}>"));
                i = j + 1;
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

/// Message text as WhatsApp shows it: escaped, with `*bold*`, `_italic_`,
/// `~struck~` and fixed-width between three backticks, and a chip for each
/// `{{value}}` nothing filled.
pub(crate) fn message_text(text: &str) -> String {
    let mut out = String::new();
    for (n, part) in escape(text).split("```").enumerate() {
        if n % 2 == 1 {
            out.push_str(&format!("<code>{part}</code>"));
        } else {
            let part = mark(part, '*', "strong");
            let part = mark(&part, '_', "em");
            out.push_str(&mark(&part, '~', "s"));
        }
    }
    chips(&out)
}

/// A chip around each `{{value}}` left in `text`.
fn chips(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start..].find("}}") else {
            break;
        };
        let key = &rest[start + 2..start + len];
        let named = !key.trim().is_empty()
            && key
                .trim()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_');
        out.push_str(&rest[..start]);
        if named {
            out.push_str(&format!(r#"<span class="wa-var">{{{{{key}}}}}</span>"#));
        } else {
            out.push_str(&rest[start..start + len + 2]);
        }
        rest = &rest[start + len + 2..];
    }
    out.push_str(rest);
    out
}

const REPLY: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 14 4 9l5-5"/><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/></svg>"#;

/// The time a preview's message carries.
pub(crate) const TIME: &str = "10:30";

/// The message bubble alone, for a preview the editor's script redraws.
pub(crate) fn bubble(shell: &Shell, preview: &Preview) -> String {
    let mut inner = String::new();
    if let Some(media) = &preview.media {
        let (icon, label) = match media.as_str() {
            "VIDEO" => ("video", t!("Video")),
            "DOCUMENT" => ("file-text", t!("Document")),
            "LOCATION" => ("map-pin", t!("Location")),
            _ => ("image", t!("Image")),
        };
        inner.push_str(&format!(
            r#"<div class="wa-bubble__media">{}<span>{}</span></div>"#,
            shell.icon(icon),
            escape(&shell.tt(&label))
        ));
    }
    inner.push_str(r#"<div class="wa-bubble__text">"#);
    if let Some(header) = &preview.header {
        inner.push_str(&format!(
            r#"<div class="wa-bubble__header">{}</div>"#,
            chips(&escape(header))
        ));
    }
    if preview.body.trim().is_empty() {
        inner.push_str(&format!(
            r#"<div class="wa-bubble__body wa-bubble__empty">{}</div>"#,
            escape(&shell.t("Your message appears here."))
        ));
    } else {
        inner.push_str(&format!(
            r#"<div class="wa-bubble__body">{}</div>"#,
            message_text(&preview.body)
        ));
    }
    if let Some(footer) = &preview.footer {
        inner.push_str(&format!(
            r#"<div class="wa-bubble__footer">{}</div>"#,
            escape(footer)
        ));
    }
    inner.push_str(&format!(
        r#"</div><div class="wa-bubble__time">{TIME}</div>"#
    ));
    for (kind, text, target) in &preview.buttons {
        let icon = match kind.as_str() {
            "URL" => shell.icon("external-link"),
            "PHONE_NUMBER" => shell.icon("phone"),
            "COPY_CODE" => shell.icon("copy"),
            _ => REPLY.to_string(),
        };
        inner.push_str(&format!(
            r#"<div class="wa-bubble__btn" title="{}">{icon}<span>{}</span></div>"#,
            escape(target),
            escape(text)
        ));
    }
    format!(r#"<div class="wa-bubble">{inner}</div>"#)
}

/// The message on a chat's wallpaper, as a phone shows it.
pub(crate) fn phone(shell: &Shell, preview: &Preview) -> String {
    format!(r#"<div class="wa-chat">{}</div>"#, bubble(shell, preview))
}

/// A category as an administrator reads it.
pub(crate) fn category_text(category: &str) -> Text {
    match category.to_ascii_uppercase().as_str() {
        "MARKETING" => t!("Marketing"),
        "UTILITY" => t!("Utility"),
        "AUTHENTICATION" => t!("Authentication"),
        "SERVICE" => t!("Service"),
        other => Text::dynamic(other.to_string()),
    }
}

/// What a category costs, in a line.
pub(crate) fn category_cost(category: &str) -> Text {
    match category.to_ascii_uppercase().as_str() {
        "MARKETING" => t!("Charged for every message, at Meta's highest rate"),
        "UTILITY" => {
            t!("Charged per message, and free in a chat the person opened in the last 24 hours")
        }
        "AUTHENTICATION" => t!("Charged for every message"),
        _ => t!("Not charged"),
    }
}

/// The square that marks a template's category.
pub(crate) fn tile(shell: &Shell, category: &str, small: bool) -> String {
    let category = category.to_ascii_uppercase();
    let icon = match category.as_str() {
        "MARKETING" => "megaphone",
        "AUTHENTICATION" => "lock",
        _ => "bell",
    };
    format!(
        r#"<span class="wa-tile{}" data-category="{}" title="{}">{}</span>"#,
        if small { " wa-tile--sm" } else { "" },
        escape(&category),
        escape(&shell.tt(&category_text(&category))),
        shell.icon(icon)
    )
}

/// A coloured state.
pub(crate) fn pill(tone: &str, label: &str) -> String {
    format!(
        r#"<span class="wa-pill" data-tone="{tone}">{}</span>"#,
        escape(label)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_become_tags_and_unfilled_values_become_chips() {
        assert_eq!(
            message_text("Hi *Ravi*, _see_ you ~soon~ at {{2}}."),
            r#"Hi <strong>Ravi</strong>, <em>see</em> you <s>soon</s> at <span class="wa-var">{{2}}</span>."#
        );
        // A sign inside a word, or one that hugs nothing, is left alone.
        assert_eq!(message_text("a*b*c 2 * 3 * 4"), "a*b*c 2 * 3 * 4");
        assert_eq!(
            message_text("{{guest_name}} <b>"),
            r#"<span class="wa-var">{{guest_name}}</span> &lt;b&gt;"#
        );
        assert_eq!(
            message_text("Use ```code_here```"),
            "Use <code>code_here</code>"
        );
    }
}
