//! Minimal Markdown-to-ratatui renderer for the changelog preview.
//!
//! Handles the constructs that appear in GitHub release notes: headings, lists,
//! code blocks, inline code, bold/italic, links, block quotes, and rules.

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// Render a Markdown string into styled lines for display.
pub fn render(markdown: &str, theme: &Theme) -> Vec<Line<'static>> {
    let mut r = Renderer::new(theme);
    for event in Parser::new(markdown) {
        r.handle(event);
    }
    r.finish()
}

struct Renderer<'t> {
    theme: &'t Theme,
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    bold: bool,
    italic: bool,
    heading: bool,
    quote: bool,
    in_code_block: bool,
    /// Stack of list markers: `Some(counter)` for ordered, `None` for bullets.
    lists: Vec<Option<u64>>,
    link: Option<String>,
}

impl<'t> Renderer<'t> {
    fn new(theme: &'t Theme) -> Self {
        Self {
            theme,
            lines: Vec::new(),
            current: Vec::new(),
            bold: false,
            italic: false,
            heading: false,
            quote: false,
            in_code_block: false,
            lists: Vec::new(),
            link: None,
        }
    }

    fn flush(&mut self) {
        if !self.current.is_empty() {
            self.lines.push(Line::from(std::mem::take(&mut self.current)));
        }
    }

    fn blank(&mut self) {
        // Avoid stacking multiple blank lines.
        if self.lines.last().map(|l| l.spans.is_empty()) != Some(true) {
            self.lines.push(Line::from(String::new()));
        }
    }

    fn text_style(&self) -> Style {
        let mut s = Style::default().fg(self.theme.fg);
        if self.heading {
            s = Style::default()
                .fg(self.theme.accent)
                .add_modifier(Modifier::BOLD);
        } else if self.quote {
            s = Style::default()
                .fg(self.theme.dim)
                .add_modifier(Modifier::ITALIC);
        }
        if self.bold {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.italic {
            s = s.add_modifier(Modifier::ITALIC);
        }
        s
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if self.in_code_block {
                    for line in t.split('\n') {
                        // Drop a trailing empty split from a final newline.
                        self.lines.push(Line::from(Span::styled(
                            format!("  {line}"),
                            Style::default().fg(self.theme.secondary),
                        )));
                    }
                } else {
                    let style = self.text_style();
                    self.current.push(Span::styled(t.into_string(), style));
                }
            }
            Event::Code(t) => {
                self.current.push(Span::styled(
                    t.into_string(),
                    Style::default().fg(self.theme.secondary),
                ));
            }
            Event::SoftBreak => self.current.push(Span::raw(" ")),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
                self.lines.push(Line::from(Span::styled(
                    "────────────────────".to_string(),
                    Style::default().fg(self.theme.border),
                )));
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { level, .. } => {
                self.flush();
                self.blank();
                self.heading = true;
                let hashes = match level {
                    HeadingLevel::H1 => "",
                    HeadingLevel::H2 => "",
                    _ => "",
                };
                if !hashes.is_empty() {
                    let style = self.text_style();
                    self.current.push(Span::styled(hashes.to_string(), style));
                }
            }
            Tag::Paragraph => self.flush(),
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote = true;
                self.current.push(Span::styled(
                    "▏ ".to_string(),
                    Style::default().fg(self.theme.border),
                ));
            }
            Tag::List(start) => self.lists.push(start),
            Tag::Item => {
                self.flush();
                let indent = "  ".repeat(self.lists.len().saturating_sub(1));
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "• ".to_string(),
                };
                self.current.push(Span::styled(
                    format!("{indent}{marker}"),
                    Style::default().fg(self.theme.accent),
                ));
            }
            Tag::CodeBlock(_) => {
                self.flush();
                self.in_code_block = true;
            }
            Tag::Strong => self.bold = true,
            Tag::Emphasis => self.italic = true,
            Tag::Link { dest_url, .. } => self.link = Some(dest_url.into_string()),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.flush();
                self.heading = false;
            }
            TagEnd::Paragraph => {
                self.flush();
                self.blank();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = false;
            }
            TagEnd::List(_) => {
                self.lists.pop();
                if self.lists.is_empty() {
                    self.blank();
                }
            }
            TagEnd::Item => self.flush(),
            TagEnd::CodeBlock => {
                self.in_code_block = false;
                self.blank();
            }
            TagEnd::Strong => self.bold = false,
            TagEnd::Emphasis => self.italic = false,
            TagEnd::Link => {
                if let Some(url) = self.link.take() {
                    self.current.push(Span::styled(
                        format!(" ({url})"),
                        Style::default().fg(self.theme.dim),
                    ));
                }
            }
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush();
        self.lines
    }
}
