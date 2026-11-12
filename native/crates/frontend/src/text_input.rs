//! A single-line text field, hand-built on gpui's IME input-handler
//! contract (`EntityInputHandler` + `Window::handle_input`).
//!
//! gpui 0.2 ships no text-input widget (the Tauri app used a plain
//! `<input>`), so this is the minimal editor the login form needs:
//! native keyboard input via the platform IME path (which also gives
//! dead-key composition), plus backspace/arrow handling from key events,
//! a caret and a placeholder. Selection, drag-select and undo are not
//! implemented — the only consumer is the 16-character offline username
//! field (`auth-panel.tsx`).

use std::ops::Range;

use gpui::{
    canvas, div, prelude::*, px, Bounds, Context, EntityInputHandler, FocusHandle,
    Focusable, KeyDownEvent, Pixels, SharedString, UTF16Selection, Window,
};

use crate::theme::{self, Theme};

/// How much utf16 (surrogate-pair aware) the byte offset `limit` covers.
fn utf16_len_before(text: &str, limit: usize) -> usize {
    text.char_indices()
        .take_while(|(i, _)| *i < limit)
        .map(|(_, c)| c.len_utf16())
        .sum()
}

/// The utf16 index back into a byte offset for this field's text.
#[allow(dead_code)]
fn byte_offset_from_utf16(text: &str, utf16_index: usize) -> usize {
    let mut utf16_seen = 0;
    for (i, c) in text.char_indices() {
        if utf16_seen >= utf16_index {
            return i;
        }
        utf16_seen += c.len_utf16();
    }
    text.len()
}

pub struct TextField {
    pub focus: FocusHandle,
    pub text: String,
    /// Caret position as a byte offset (always on a char boundary).
    cursor: usize,
    pub placeholder: SharedString,
}

impl TextField {
    pub fn new(cx: &mut Context<Self>, placeholder: &str) -> Self {
        Self {
            focus: cx.focus_handle(),
            text: String::new(),
            cursor: 0,
            placeholder: SharedString::from(placeholder.to_string()),
        }
    }

    pub fn value(&self) -> &str {
        &self.text
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    fn insert_str(&mut self, s: &str, cx: &mut Context<Self>) {
        if s.is_empty() {
            return;
        }
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.changed(cx);
    }

    fn backspace(&mut self, cx: &mut Context<Self>) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.text.replace_range(prev..self.cursor, "");
        self.cursor = prev;
        self.changed(cx);
    }

    fn move_cursor(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.cursor = if forward {
            self.text[self.cursor..]
                .chars()
                .next()
                .map(|c| self.cursor + c.len_utf8())
                .unwrap_or(self.cursor)
        } else {
            self.text[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(self.cursor)
        };
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.contains_focused(window, cx) {
            return;
        }
        match event.keystroke.key.as_str() {
            "backspace" => {
                if event.keystroke.modifiers.control {
                    // Ctrl+Backspace: delete the word before the caret.
                    let mut end = self.cursor;
                    let take_word = |text: &str, mut end: usize, pred: &dyn Fn(char) -> bool| {
                        while let Some((i, c)) = text[..end].char_indices().next_back() {
                            if pred(c) {
                                end = i;
                            } else {
                                break;
                            }
                        }
                        end
                    };
                    // Skip whitespace, then the word characters.
                    end = take_word(&self.text, end, &|c: char| !c.is_alphanumeric());
                    end = take_word(&self.text, end, &|c: char| c.is_alphanumeric());
                    self.text.replace_range(end..self.cursor, "");
                    self.cursor = end;
                    self.changed(cx);
                } else {
                    self.backspace(cx);
                }
            }
            "delete" => {
                if let Some(c) = self.text[self.cursor..].chars().next() {
                    let end = self.cursor + c.len_utf8();
                    self.text.replace_range(self.cursor..end, "");
                    self.changed(cx);
                }
            }
            "left" => self.move_cursor(false, cx),
            "right" => self.move_cursor(true, cx),
            "home" => {
                self.cursor = 0;
                cx.notify();
            }
            "end" => {
                self.cursor = self.text.len();
                cx.notify();
            }
            _ => {}
        }
    }

    /// Renders the styled field (glass-input, h-10, rounded-xl, px-3,
    /// 14px text). The parent supplies width (flex-1 etc.). An invisible
    /// canvas over the field registers the IME input handler during paint,
    /// which is what routes native keyboard characters (and IME
    /// composition) into `replace_text_in_range`.
    pub fn render_field(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = *Theme::get(cx);
        let focused = self.focus.is_focused(window);
        let (bg, border) = if focused {
            (theme::white(0.05), theme.border_strong) // :focus style
        } else {
            (theme::white(0.03), theme.border)
        };
        let entity = cx.entity();
        let placeholder_color = theme.muted;

        let mut field = div()
            .id("username-input")
            .relative()
            .h(px(40.0)) // h-10
            .flex()
            .items_center()
            .rounded(px(12.0)) // rounded-xl
            .px(px(12.0)) // px-3
            .text_size(px(14.0)) // text-sm
            .text_color(theme.foreground)
            .bg(bg)
            .border_1()
            .border_color(border)
            .track_focus(&self.focus)
            .key_context("UsernameInput")
            .on_click(cx.listener(|this, _, window, _cx| {
                window.focus(&this.focus);
            }))
            .on_key_down(cx.listener(Self::on_key_down))
            .child(if self.text.is_empty() {
                div()
                    .text_color(placeholder_color)
                    .child(self.placeholder.clone())
                    .into_any_element()
            } else {
                div().child(self.text.clone()).into_any_element()
            });

        if focused {
            // Caret: a 1px bar after the text (a static one — the CSS
            // caret's blink is the only missing bit here).
            field = field.child(
                div()
                    .w(px(1.0))
                    .h(px(18.0))
                    .bg(theme.foreground),
            );
        }

        field
            .child(canvas(
                // Register the input handler under the field's bounds.
                {
                    let entity = entity.clone();
                    move |bounds, window, cx| {
                        let focus = entity.read(cx).focus.clone();
                        window.handle_input(
                            &focus,
                            gpui::ElementInputHandler::new(bounds, entity),
                            cx,
                        );
                    }
                },
                |_, _, _, _| {},
            ))
            .into_any_element()
    }
}

impl Focusable for TextField {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        Some(self.text.get(range)?.to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let caret_utf16 = utf16_len_before(&self.text, self.cursor);
        Some(UTF16Selection {
            range: caret_utf16..caret_utf16,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Native chars (incl. IME composition results) arrive here. A
        // caret-only field replaces nothing — plain insertion; "\u{8}" is
        // how some platforms deliver backspace.
        if text == "\u{8}" || text == "\u{7f}" {
            self.backspace(cx);
            return;
        }
        // Strip control chars (enter would be wrong in a single-line
        // field); the username grammar is plain printable text.
        let cleaned: String = text.chars().filter(|c| !c.is_control()).collect();
        self.insert_str(&cleaned, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_text_in_range(range, new_text, window, cx);
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // Used only for IME popup placement — the caret is always at the
        // end of what was typed for this field.
        Some(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
