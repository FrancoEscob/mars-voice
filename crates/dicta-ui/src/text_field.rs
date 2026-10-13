//! Native text field for GPUI, which ships no input widget.
//!
//! [`EditBuffer`] holds the editing rules and is tested on its own. [`TextField`]
//! wraps it in an entity that handles focus, IME, clipboard and mouse, and
//! paints through [`FieldElement`]. Fields emit [`FieldEvent::Commit`] on Enter
//! (Ctrl+Enter when multi-line) and on blur, so owners only persist settled
//! values instead of writing configuration on every keystroke.
//!
//! A multi-line field built with [`TextField::fill_height`] takes the height
//! its parent gives it and scrolls vertically by itself: hit-testing, IME
//! bounds, selection and the caret all account for the offset, and the view
//! follows the caret whenever it moves (typing, arrows, drag-selecting past
//! an edge, new text from a snapshot) without undoing manual wheel scrolling.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ScrollWheelEvent, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window,
    WrappedLine, actions, div, fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{i18n::Msg, perf, ui_state};

/// Hint shown in an empty field: fixed text (a code, a key, a number) or a
/// message resolved in the current interface language at paint time, so a
/// language switch reaches open fields.
#[derive(Clone, Debug)]
pub enum Placeholder {
    Text(SharedString),
    Msg(Msg),
}

impl From<&'static str> for Placeholder {
    fn from(text: &'static str) -> Self {
        Self::Text(text.into())
    }
}

impl From<String> for Placeholder {
    fn from(text: String) -> Self {
        Self::Text(text.into())
    }
}

impl From<Msg> for Placeholder {
    fn from(m: Msg) -> Self {
        Self::Msg(m)
    }
}

actions!(
    dicta_field,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        WordLeft,
        WordRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
        Enter,
        Newline,
        CommitField,
        Blur,
    ]
);

const CONTEXT: &str = "DictaField";
const MASK: char = '•';

/// Word-wise caret moves: Option on macOS, Ctrl elsewhere.
#[cfg(target_os = "macos")]
const WORD_LEFT: &str = "alt-left";
#[cfg(target_os = "macos")]
const WORD_RIGHT: &str = "alt-right";
#[cfg(not(target_os = "macos"))]
const WORD_LEFT: &str = "ctrl-left";
#[cfg(not(target_os = "macos"))]
const WORD_RIGHT: &str = "ctrl-right";

/// Editing keys. `secondary-` is Cmd on macOS and Ctrl elsewhere, so select
/// all, clipboard and commit follow the platform.
pub fn bind_keys(cx: &mut App) {
    let c = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("left", Left, c),
        KeyBinding::new("right", Right, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("shift-up", SelectUp, c),
        KeyBinding::new("shift-down", SelectDown, c),
        KeyBinding::new(WORD_LEFT, WordLeft, c),
        KeyBinding::new(WORD_RIGHT, WordRight, c),
        KeyBinding::new("secondary-a", SelectAll, c),
        KeyBinding::new("home", Home, c),
        KeyBinding::new("end", End, c),
        KeyBinding::new("secondary-v", Paste, c),
        KeyBinding::new("shift-insert", Paste, c),
        KeyBinding::new("secondary-x", Cut, c),
        KeyBinding::new("secondary-c", Copy, c),
        KeyBinding::new("enter", Enter, c),
        KeyBinding::new("shift-enter", Newline, c),
        KeyBinding::new("secondary-enter", CommitField, c),
        KeyBinding::new("escape", Blur, c),
    ]);
}

/// Pure editing state: text plus a selection in UTF-8 byte offsets that always
/// sit on grapheme boundaries.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditBuffer {
    pub text: String,
    pub selection: Range<usize>,
    pub reversed: bool,
}

impl EditBuffer {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let end = text.len();
        Self {
            text,
            selection: end..end,
            reversed: false,
        }
    }

    pub fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    pub fn selected(&self) -> &str {
        &self.text[self.selection.clone()]
    }

    pub fn move_to(&mut self, offset: usize) {
        let offset = offset.min(self.text.len());
        self.selection = offset..offset;
        self.reversed = false;
    }

    pub fn select_to(&mut self, offset: usize) {
        let offset = offset.min(self.text.len());
        if self.reversed {
            self.selection.start = offset;
        } else {
            self.selection.end = offset;
        }
        if self.selection.end < self.selection.start {
            self.reversed = !self.reversed;
            self.selection = self.selection.end..self.selection.start;
        }
    }

    pub fn select_all(&mut self) {
        self.selection = 0..self.text.len();
        self.reversed = false;
    }

    pub fn prev_boundary(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .rev()
            .find_map(|(i, _)| (i < offset).then_some(i))
            .unwrap_or(0)
    }

    pub fn next_boundary(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .find_map(|(i, _)| (i > offset).then_some(i))
            .unwrap_or(self.text.len())
    }

    pub fn prev_word(&self, offset: usize) -> usize {
        self.text
            .split_word_bound_indices()
            .rev()
            .find_map(|(i, w)| (i < offset && !w.trim().is_empty()).then_some(i))
            .unwrap_or(0)
    }

    pub fn next_word(&self, offset: usize) -> usize {
        self.text
            .split_word_bound_indices()
            .find_map(|(i, w)| {
                let end = i + w.len();
                (end > offset && !w.trim().is_empty()).then_some(end)
            })
            .unwrap_or(self.text.len())
    }

    pub fn line_start(&self, offset: usize) -> usize {
        self.text[..offset].rfind('\n').map_or(0, |i| i + 1)
    }

    pub fn line_end(&self, offset: usize) -> usize {
        self.text[offset..]
            .find('\n')
            .map_or(self.text.len(), |i| offset + i)
    }

    pub fn left(&mut self) {
        if self.selection.is_empty() {
            self.move_to(self.prev_boundary(self.cursor()));
        } else {
            self.move_to(self.selection.start);
        }
    }

    pub fn right(&mut self) {
        if self.selection.is_empty() {
            self.move_to(self.next_boundary(self.cursor()));
        } else {
            self.move_to(self.selection.end);
        }
    }

    /// Replaces `range` (or the selection) and leaves the cursor after the
    /// inserted text.
    pub fn replace(&mut self, range: Option<Range<usize>>, new_text: &str) {
        let range = range.unwrap_or_else(|| self.selection.clone());
        let range = range.start.min(self.text.len())..range.end.min(self.text.len());
        self.text.replace_range(range.clone(), new_text);
        self.move_to(range.start + new_text.len());
    }

    /// Returns false when there was nothing to delete.
    pub fn backspace(&mut self) -> bool {
        if self.selection.is_empty() {
            let prev = self.prev_boundary(self.cursor());
            if prev == self.cursor() {
                return false;
            }
            self.select_to(prev);
        }
        self.replace(None, "");
        true
    }

    pub fn delete(&mut self) -> bool {
        if self.selection.is_empty() {
            let next = self.next_boundary(self.cursor());
            if next == self.cursor() {
                return false;
            }
            self.select_to(next);
        }
        self.replace(None, "");
        true
    }

    pub fn offset_to_utf16(&self, offset: usize) -> usize {
        self.text[..offset.min(self.text.len())]
            .encode_utf16()
            .count()
    }

    pub fn offset_from_utf16(&self, utf16: usize) -> usize {
        let mut count = 0;
        for (i, ch) in self.text.char_indices() {
            if count >= utf16 {
                return i;
            }
            count += ch.len_utf16();
        }
        self.text.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldEvent {
    /// The value changed; owners may preview it.
    Changed,
    /// The user settled the value (Enter, Ctrl+Enter or focus left).
    Commit,
}

pub struct TextField {
    focus: FocusHandle,
    buffer: EditBuffer,
    marked: Option<Range<usize>>,
    placeholder: Placeholder,
    multiline: bool,
    fill: bool,
    masked: bool,
    dirty: bool,
    selecting: bool,
    scroll_x: Pixels,
    scroll_y: Pixels,
    /// Caret position and text length at the last paint; a change scrolls
    /// the caret into view.
    painted_caret: Option<(usize, usize)>,
    layout: Option<FieldLayout>,
    _blur: Option<gpui::Subscription>,
}

impl EventEmitter<FieldEvent> for TextField {}

impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TextField {
    pub fn new(
        text: impl Into<String>,
        placeholder: impl Into<Placeholder>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let blur = cx.on_blur(&focus, window, |this, _, cx| this.commit(cx));
        Self {
            focus,
            buffer: EditBuffer::new(text),
            marked: None,
            placeholder: placeholder.into(),
            multiline: false,
            fill: false,
            masked: false,
            dirty: false,
            selecting: false,
            scroll_x: px(0.),
            scroll_y: px(0.),
            painted_caret: None,
            layout: None,
            _blur: Some(blur),
        }
    }

    pub fn multiline(mut self) -> Self {
        self.multiline = true;
        self
    }

    /// Multi-line only: fill the parent's height and scroll inside it
    /// instead of growing with the text.
    pub fn fill_height(mut self) -> Self {
        self.fill = true;
        self
    }

    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    pub fn text(&self) -> &str {
        &self.buffer.text
    }

    /// The placeholder in the current interface language.
    fn placeholder_text(&self, cx: &App) -> SharedString {
        match &self.placeholder {
            Placeholder::Text(text) => text.clone(),
            Placeholder::Msg(m) => ui_state(cx).lang.t(*m).into(),
        }
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// Replaces the value from outside (a new snapshot). Ignored while the
    /// user has unsaved edits, so incoming snapshots never eat typing.
    pub fn sync(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.dirty || self.buffer.text == text {
            return;
        }
        self.buffer = EditBuffer::new(text);
        self.marked = None;
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.buffer = EditBuffer::new("");
        self.marked = None;
        self.dirty = false;
        cx.notify();
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        if self.dirty {
            self.dirty = false;
            cx.emit(FieldEvent::Commit);
        }
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
        cx.emit(FieldEvent::Changed);
        cx.notify();
    }

    fn sanitize<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        if self.multiline {
            text.into()
        } else {
            text.replace(['\n', '\r'], " ").into()
        }
    }

    // Masked fields paint one bullet per char; these map between real and
    // painted byte offsets. Identity otherwise.
    fn offset_to_display(&self, offset: usize) -> usize {
        if !self.masked {
            return offset;
        }
        self.buffer.text[..offset.min(self.buffer.text.len())]
            .chars()
            .count()
            * MASK.len_utf8()
    }

    fn display_to_offset(&self, offset: usize) -> usize {
        if !self.masked {
            return offset;
        }
        let n = offset / MASK.len_utf8();
        self.buffer
            .text
            .char_indices()
            .nth(n)
            .map_or(self.buffer.text.len(), |(i, _)| i)
    }

    fn display_text(&self) -> SharedString {
        if self.masked {
            MASK.to_string()
                .repeat(self.buffer.text.chars().count())
                .into()
        } else {
            self.buffer.text.clone().into()
        }
    }

    fn index_for_point(&self, position: Point<Pixels>) -> usize {
        let Some(layout) = self.layout.as_ref() else {
            return self.buffer.text.len();
        };
        self.display_to_offset(layout.index_for_point(position))
    }

    fn vertical(&mut self, delta: isize, select: bool, cx: &mut Context<Self>) {
        let target = self
            .layout
            .as_ref()
            .and_then(|l| l.vertical_index(self.offset_to_display(self.buffer.cursor()), delta));
        let target = match target {
            Some(t) => self.display_to_offset(t),
            None if delta < 0 => 0,
            None => self.buffer.text.len(),
        };
        if select {
            self.buffer.select_to(target)
        } else {
            self.buffer.move_to(target)
        }
        cx.notify();
    }

    fn on_backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.backspace() {
            self.edited(cx);
        }
    }
    fn on_delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.delete() {
            self.edited(cx);
        }
    }
    fn on_left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.left();
        cx.notify();
    }
    fn on_right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.right();
        cx.notify();
    }
    fn on_up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(-1, false, cx);
    }
    fn on_down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(1, false, cx);
    }
    fn on_select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(-1, true, cx);
    }
    fn on_select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(1, true, cx);
    }
    fn on_select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer
            .select_to(self.buffer.prev_boundary(self.buffer.cursor()));
        cx.notify();
    }
    fn on_select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer
            .select_to(self.buffer.next_boundary(self.buffer.cursor()));
        cx.notify();
    }
    fn on_word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer
            .move_to(self.buffer.prev_word(self.buffer.cursor()));
        cx.notify();
    }
    fn on_word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer
            .move_to(self.buffer.next_word(self.buffer.cursor()));
        cx.notify();
    }
    fn on_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.select_all();
        cx.notify();
    }
    fn on_home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer
            .move_to(self.buffer.line_start(self.buffer.cursor()));
        cx.notify();
    }
    fn on_end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer
            .move_to(self.buffer.line_end(self.buffer.cursor()));
        cx.notify();
    }
    fn on_paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = self.sanitize(&text).into_owned();
            self.buffer.replace(None, &text);
            self.edited(cx);
        }
    }
    fn on_copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.masked && !self.buffer.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.buffer.selected().to_string(),
            ));
        }
    }
    fn on_cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if self.masked || self.buffer.selection.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(
            self.buffer.selected().to_string(),
        ));
        self.buffer.replace(None, "");
        self.edited(cx);
    }
    fn on_enter(&mut self, _: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            self.on_newline(&Newline, window, cx);
        } else {
            self.dirty = true;
            self.commit(cx);
        }
    }
    fn on_newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            self.buffer.replace(None, "\n");
            self.edited(cx);
        }
    }
    fn on_commit(&mut self, _: &CommitField, _: &mut Window, cx: &mut Context<Self>) {
        self.dirty = true;
        self.commit(cx);
    }
    fn on_blur_action(&mut self, _: &Blur, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(cx);
        window.blur(cx);
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        self.selecting = true;
        let index = self.index_for_point(event.position);
        if event.modifiers.shift {
            self.buffer.select_to(index);
        } else if event.click_count >= 2 {
            let start = self.buffer.prev_word(self.buffer.next_boundary(index));
            self.buffer.move_to(start);
            self.buffer.select_to(self.buffer.next_word(start));
        } else {
            self.buffer.move_to(index);
        }
        cx.notify();
    }
    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }
    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.buffer.select_to(self.index_for_point(event.position));
            cx.notify();
        }
    }
    fn on_scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let max = layout.max_scroll_y();
        if max <= px(0.) {
            return;
        }
        let delta = event.delta.pixel_delta(window.line_height()).y;
        self.scroll_y = (self.scroll_y - delta).clamp(px(0.), max);
        cx.stop_propagation();
        cx.notify();
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        if self.masked {
            return None;
        }
        let range = self.buffer.offset_from_utf16(range_utf16.start)
            ..self.buffer.offset_from_utf16(range_utf16.end);
        actual_range.replace(
            self.buffer.offset_to_utf16(range.start)..self.buffer.offset_to_utf16(range.end),
        );
        Some(self.buffer.text[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let s = &self.buffer.selection;
        Some(UTF16Selection {
            range: self.buffer.offset_to_utf16(s.start)..self.buffer.offset_to_utf16(s.end),
            reversed: self.buffer.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|r| self.buffer.offset_to_utf16(r.start)..self.buffer.offset_to_utf16(r.end))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .map(|r| self.buffer.offset_from_utf16(r.start)..self.buffer.offset_from_utf16(r.end))
            .or(self.marked.clone());
        let text = self.sanitize(new_text).into_owned();
        self.buffer.replace(range, &text);
        self.marked = None;
        self.edited(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .map(|r| self.buffer.offset_from_utf16(r.start)..self.buffer.offset_from_utf16(r.end))
            .or(self.marked.clone())
            .unwrap_or(self.buffer.selection.clone());
        self.buffer.replace(Some(range.clone()), new_text);
        self.marked = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        if let Some(sel) = new_selected_range_utf16 {
            // Offsets are relative to the inserted text.
            let inserted = &new_text;
            let to_utf8 = |u: usize| {
                let mut count = 0;
                for (i, ch) in inserted.char_indices() {
                    if count >= u {
                        return i;
                    }
                    count += ch.len_utf16();
                }
                inserted.len()
            };
            self.buffer.selection =
                range.start + to_utf8(sel.start)..range.start + to_utf8(sel.end);
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let start = self.offset_to_display(self.buffer.offset_from_utf16(range_utf16.start));
        let end = self.offset_to_display(self.buffer.offset_from_utf16(range_utf16.end));
        let a = layout.point_for_index(start)?;
        let b = layout.point_for_index(end)?;
        Some(Bounds::from_corners(
            point(element_bounds.left() + a.x, element_bounds.top() + a.y),
            point(
                element_bounds.left() + b.x.max(a.x),
                element_bounds.top() + b.y + layout.line_height,
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let index = self.index_for_point(point);
        Some(self.buffer.offset_to_utf16(index))
    }
}

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui_state(cx).palette;
        let focused = self.focus.is_focused(window);
        div()
            .id("field")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::on_backspace))
            .on_action(cx.listener(Self::on_delete))
            .on_action(cx.listener(Self::on_left))
            .on_action(cx.listener(Self::on_right))
            .on_action(cx.listener(Self::on_up))
            .on_action(cx.listener(Self::on_down))
            .on_action(cx.listener(Self::on_select_up))
            .on_action(cx.listener(Self::on_select_down))
            .on_action(cx.listener(Self::on_select_left))
            .on_action(cx.listener(Self::on_select_right))
            .on_action(cx.listener(Self::on_word_left))
            .on_action(cx.listener(Self::on_word_right))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::on_home))
            .on_action(cx.listener(Self::on_end))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_cut))
            .on_action(cx.listener(Self::on_enter))
            .on_action(cx.listener(Self::on_newline))
            .on_action(cx.listener(Self::on_commit))
            .on_action(cx.listener(Self::on_blur_action))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .when(self.fill && self.multiline, |d| {
                d.on_scroll_wheel(cx.listener(Self::on_scroll))
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
            })
            .w_full()
            .px(px(10.))
            .py(px(7.))
            .rounded(px(9.))
            .border_1()
            .border_color(if focused { p.text } else { p.control })
            .when(focused, |d| d.shadow(vec![p.focus_halo()]))
            .when(!focused, |d| d.hover(move |s| s.border_color(p.text_2)))
            .bg(p.raised)
            .text_color(p.text)
            .when(self.multiline, |d| d.min_h(px(64.)))
            .child(FieldElement { field: cx.entity() })
    }
}

/// Shaped text from the last paint, reused for hit-testing and IME bounds.
struct FieldLayout {
    lines: Vec<WrappedLine>,
    /// Display byte offset where each paragraph starts.
    starts: Vec<usize>,
    /// Row start offsets (paragraph-local) for each paragraph.
    rows: Vec<Vec<usize>>,
    line_height: Pixels,
    origin: Point<Pixels>,
    scroll_x: Pixels,
    scroll_y: Pixels,
    /// Visible height, for clamping the vertical scroll.
    viewport: Pixels,
}

/// Largest vertical scroll, rounded up to whole rows so the first visible
/// row is never cut in half when the view sits at the end.
fn max_scroll(content: Pixels, viewport: Pixels, line_height: Pixels) -> Pixels {
    let overflow = content - viewport;
    if overflow <= px(0.) || line_height <= px(0.) {
        return px(0.);
    }
    line_height * (overflow / line_height).ceil()
}

/// Smallest scroll change that shows the row starting at `y` (unscrolled
/// content coordinates). Scrolling down lands on a row boundary.
fn scroll_to_show(
    y: Pixels,
    current: Pixels,
    line_height: Pixels,
    viewport: Pixels,
    content: Pixels,
) -> Pixels {
    let target = if y < current {
        y
    } else if y + line_height > current + viewport {
        let needed = y + line_height - viewport;
        if line_height > px(0.) {
            line_height * (needed / line_height).ceil()
        } else {
            needed
        }
    } else {
        current
    };
    target.clamp(px(0.), max_scroll(content, viewport, line_height))
}

/// Last offset a click can land on in the row `start..next` of `text`.
/// `next` (the following row's start) is drawn at the start of that row, so
/// a click past the end of a wrapped row stops one character earlier.
fn row_hit_end(text: &str, start: usize, next: Option<usize>) -> usize {
    match next {
        Some(next) => text[..next]
            .char_indices()
            .next_back()
            .map_or(next, |(i, _)| i.max(start)),
        None => text.len(),
    }
}

fn row_starts(line: &WrappedLine) -> Vec<usize> {
    let mut rows = vec![0];
    for b in line.wrap_boundaries() {
        rows.push(line.unwrapped_layout.runs[b.run_ix].glyphs[b.glyph_ix].index);
    }
    rows
}

impl FieldLayout {
    fn locate(&self, index: usize) -> (usize, usize, usize) {
        let para = self.starts.iter().rposition(|&s| s <= index).unwrap_or(0);
        let local = (index - self.starts[para]).min(self.lines[para].len());
        let row = self.rows[para]
            .iter()
            .rposition(|&s| s <= local)
            .unwrap_or(0);
        (para, row, local)
    }

    fn rows_before(&self, para: usize) -> usize {
        self.rows[..para].iter().map(Vec::len).sum()
    }

    fn content_height(&self) -> Pixels {
        self.line_height * self.rows.iter().map(Vec::len).sum::<usize>().max(1) as f32
    }

    fn max_scroll_y(&self) -> Pixels {
        max_scroll(self.content_height(), self.viewport, self.line_height)
    }

    /// Position relative to the element origin (scroll already applied).
    fn point_for_index(&self, index: usize) -> Option<Point<Pixels>> {
        if self.lines.is_empty() {
            return Some(point(px(0.), px(0.)));
        }
        let (para, row, local) = self.locate(index);
        let line = &self.lines[para];
        let u = &line.unwrapped_layout;
        let x = u.x_for_index(local) - u.x_for_index(self.rows[para][row]);
        let y = self.line_height * (self.rows_before(para) + row) as f32;
        Some(point(x - self.scroll_x, y - self.scroll_y))
    }

    fn index_at(&self, para: usize, row: usize, x: Pixels) -> usize {
        let line = &self.lines[para];
        let rows = &self.rows[para];
        let u = &line.unwrapped_layout;
        let start = rows[row];
        let end = row_hit_end(&line.text, start, rows.get(row + 1).copied()).min(line.len());
        let local = u
            .closest_index_for_x(x + u.x_for_index(start))
            .clamp(start, end);
        self.starts[para] + local
    }

    /// `position` is in window coordinates.
    fn index_for_point(&self, position: Point<Pixels>) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        let local = position - self.origin + point(px(0.), self.scroll_y);
        let total: usize = self.rows.iter().map(Vec::len).sum();
        let target = if local.y < px(0.) {
            0
        } else {
            ((local.y / self.line_height) as usize).min(total - 1)
        };
        let mut seen = 0;
        for (para, rows) in self.rows.iter().enumerate() {
            if target < seen + rows.len() {
                return self.index_at(para, target - seen, local.x + self.scroll_x);
            }
            seen += rows.len();
        }
        self.starts.last().copied().unwrap_or(0)
    }

    fn vertical_index(&self, index: usize, delta: isize) -> Option<usize> {
        if self.lines.is_empty() {
            return None;
        }
        let (para, row, _) = self.locate(index);
        let x = self.point_for_index(index)?.x + self.scroll_x;
        let flat: Vec<(usize, usize)> = self
            .rows
            .iter()
            .enumerate()
            .flat_map(|(p, rows)| (0..rows.len()).map(move |r| (p, r)))
            .collect();
        let current = flat.iter().position(|&pr| pr == (para, row))?;
        let next = current
            .checked_add_signed(delta)
            .filter(|&n| n < flat.len())?;
        let (p, r) = flat[next];
        Some(self.index_at(p, r, x))
    }
}

pub struct FieldElement {
    field: Entity<TextField>,
}

pub struct FieldPrepaint {
    layout: FieldLayout,
    /// Caret offset and text length this frame was laid out for.
    caret: (usize, usize),
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
}

impl IntoElement for FieldElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl FieldElement {
    fn runs(
        text: &SharedString,
        color: gpui::Hsla,
        marked: Option<Range<usize>>,
        window: &Window,
    ) -> Vec<TextRun> {
        let style = window.text_style();
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        match marked {
            Some(m) if m.end <= text.len() => [
                TextRun {
                    len: m.start,
                    ..run.clone()
                },
                TextRun {
                    len: m.end - m.start,
                    underline: Some(UnderlineStyle {
                        color: Some(color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: text.len() - m.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            _ => vec![run],
        }
    }
}

fn shape(
    text: &SharedString,
    runs: &[TextRun],
    wrap: Option<Pixels>,
    window: &Window,
) -> Vec<WrappedLine> {
    let font_size = window.text_style().font_size.to_pixels(window.rem_size());
    window
        .text_system()
        .shape_text(text.clone(), font_size, runs, wrap, None)
        .map(|lines| lines.into_iter().collect())
        .unwrap_or_default()
}

impl Element for FieldElement {
    type RequestLayoutState = ();
    type PrepaintState = FieldPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let field = self.field.read(cx);
        let line_height = window.line_height();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        if !field.multiline {
            style.size.height = line_height.into();
            return (window.request_layout(style, [], cx), ());
        }
        if field.fill {
            // Take the height left in the field box; text scrolls inside.
            style.flex_grow = 1.;
            style.flex_shrink = 1.;
            style.flex_basis = px(0.).into();
            style.min_size.height = line_height.into();
            return (window.request_layout(style, [], cx), ());
        }
        let text = if field.buffer.text.is_empty() {
            field.placeholder_text(cx)
        } else {
            field.display_text()
        };
        let runs = Self::runs(&text, gpui::black(), None, window);
        let id = window.request_measured_layout(style, move |known, available, window, _| {
            let width = known.width.or(match available.width {
                gpui::AvailableSpace::Definite(w) => Some(w),
                _ => None,
            });
            let lines = shape(&text, &runs, width, window);
            let rows: usize = lines.iter().map(|l| l.wrap_boundaries().len() + 1).sum();
            size(width.unwrap_or(px(200.)), line_height * rows.max(1) as f32)
        });
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> FieldPrepaint {
        perf::bump(&perf::FIELD_PREPAINTS);
        let palette = ui_state(cx).palette;
        let field = self.field.read(cx);
        let line_height = window.line_height();
        let placeholder = field.buffer.text.is_empty();
        let (text, color) = if placeholder {
            (field.placeholder_text(cx), palette.tentative.into())
        } else {
            (field.display_text(), window.text_style().color)
        };
        let marked = field
            .marked
            .clone()
            .map(|m| field.offset_to_display(m.start)..field.offset_to_display(m.end));
        let runs = Self::runs(
            &text,
            color,
            if placeholder { None } else { marked },
            window,
        );
        let wrap = field.multiline.then_some(bounds.size.width);
        let lines = shape(&text, &runs, wrap, window);
        let mut starts = Vec::with_capacity(lines.len());
        let mut offset = 0;
        for line in &lines {
            starts.push(offset);
            offset += line.len() + 1;
        }
        let rows = lines.iter().map(row_starts).collect();
        let mut layout = FieldLayout {
            lines,
            starts,
            rows,
            line_height,
            origin: bounds.origin,
            scroll_x: field.scroll_x,
            scroll_y: px(0.),
            viewport: bounds.size.height,
        };
        let (cursor_ix, sel) = if placeholder {
            (0, 0..0)
        } else {
            (
                field.offset_to_display(field.buffer.cursor()),
                field.offset_to_display(field.buffer.selection.start)
                    ..field.offset_to_display(field.buffer.selection.end),
            )
        };

        // Keep the caret visible: vertically in filled multi-line fields when
        // it moved or the text changed, horizontally in single-line ones.
        if field.multiline && field.fill {
            let caret = (cursor_ix, text.len());
            let current = field.scroll_y.clamp(px(0.), layout.max_scroll_y());
            layout.scroll_y = if field.painted_caret != Some(caret) {
                let y = layout.point_for_index(cursor_ix).map_or(px(0.), |p| p.y);
                scroll_to_show(
                    y,
                    current,
                    line_height,
                    layout.viewport,
                    layout.content_height(),
                )
            } else {
                current
            };
        }
        if !field.multiline {
            let x = layout
                .point_for_index(cursor_ix)
                .map_or(px(0.), |p| p.x + layout.scroll_x);
            let width = bounds.size.width - px(2.);
            if x - layout.scroll_x > width {
                layout.scroll_x = x - width;
            } else if x < layout.scroll_x {
                layout.scroll_x = x;
            }
        }

        let caret = palette.orange;
        let cursor = layout.point_for_index(cursor_ix).map(|p| {
            fill(
                Bounds::new(
                    point(bounds.left() + p.x, bounds.top() + p.y),
                    size(px(2.), line_height),
                ),
                caret,
            )
        });
        let mut selections = Vec::new();
        if !sel.is_empty() {
            let mut sel_color = palette.orange;
            sel_color.a = 0.28;
            for (para, rows) in layout.rows.iter().enumerate() {
                let pstart = layout.starts[para];
                let pend = pstart + layout.lines[para].len();
                for (row, &rs) in rows.iter().enumerate() {
                    let rstart = pstart + rs;
                    let rend = rows.get(row + 1).map_or(pend, |&n| pstart + n);
                    let a = sel.start.max(rstart);
                    let b = sel.end.min(rend);
                    // Include the newline of fully selected paragraphs as a small block.
                    if a > b || (a == b && !(sel.start <= rend && sel.end > rend)) {
                        continue;
                    }
                    let u = &layout.lines[para].unwrapped_layout;
                    let x0 = u.x_for_index(a - pstart) - u.x_for_index(rs) - layout.scroll_x;
                    let mut x1 = u.x_for_index(b - pstart) - u.x_for_index(rs) - layout.scroll_x;
                    if sel.end > rend {
                        x1 += px(5.);
                    }
                    let y = line_height * (layout.rows_before(para) + row) as f32 - layout.scroll_y;
                    selections.push(fill(
                        Bounds::from_corners(
                            point(bounds.left() + x0, bounds.top() + y),
                            point(bounds.left() + x1, bounds.top() + y + line_height),
                        ),
                        sel_color,
                    ));
                }
            }
        }
        FieldPrepaint {
            caret: (cursor_ix, text.len()),
            layout,
            cursor: sel.is_empty().then_some(cursor).flatten(),
            selections,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut FieldPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.field.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.field.clone()),
            cx,
        );
        let layout = std::mem::replace(
            &mut prepaint.layout,
            FieldLayout {
                lines: Vec::new(),
                starts: Vec::new(),
                rows: Vec::new(),
                line_height: px(0.),
                origin: bounds.origin,
                scroll_x: px(0.),
                scroll_y: px(0.),
                viewport: px(0.),
            },
        );
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for quad in prepaint.selections.drain(..) {
                window.paint_quad(quad);
            }
            let mut y = -layout.scroll_y;
            for line in &layout.lines {
                let origin = point(bounds.left() - layout.scroll_x, bounds.top() + y);
                line.paint(
                    origin,
                    layout.line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .ok();
                y += layout.line_height * (line.wrap_boundaries().len() + 1) as f32;
            }
            if focus.is_focused(window)
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });
        let caret = prepaint.caret;
        self.field.update(cx, |field, _| {
            field.scroll_x = layout.scroll_x;
            field.scroll_y = layout.scroll_y;
            field.painted_caret = Some(caret);
            field.layout = Some(layout);
        });
    }
}

#[cfg(test)]
mod tests {
    use gpui::px;

    use super::{EditBuffer, max_scroll, row_hit_end, scroll_to_show};

    #[test]
    fn caret_below_the_view_scrolls_to_a_whole_row() {
        let (lh, vp, content) = (px(21.), px(100.), px(21. * 12.));
        // Caret on row 7 (y = 147) with the view at the top.
        let y = scroll_to_show(px(147.), px(0.), lh, vp, content);
        assert_eq!(y, px(84.), "four rows down, first visible row whole");
        assert!(px(147.) + lh <= y + vp, "caret row fully visible");
        // Caret already visible: a wheel-scrolled view is left alone.
        assert_eq!(scroll_to_show(px(63.), px(30.), lh, vp, content), px(30.));
        // Caret above the view (Home, Up, drag-select past the top edge).
        assert_eq!(scroll_to_show(px(21.), px(84.), lh, vp, content), px(21.));
    }

    #[test]
    fn scroll_stops_at_the_end_of_the_text() {
        let (lh, vp) = (px(21.), px(100.));
        assert_eq!(
            max_scroll(px(80.), vp, lh),
            px(0.),
            "short text never scrolls"
        );
        // 12 rows = 252 px; overflow 152 rounds up to 8 rows.
        assert_eq!(max_scroll(px(252.), vp, lh), px(168.));
        let last = scroll_to_show(px(231.), px(0.), lh, vp, px(252.));
        assert_eq!(last, px(168.));
    }

    #[test]
    fn clicks_past_a_wrapped_row_stay_on_that_row() {
        let text = "hola mundo largo";
        // Rows "hola " and "mundo largo": a click at the end of row 0 lands
        // before the wrapping space, not at the start of "mundo".
        assert_eq!(row_hit_end(text, 0, Some(5)), 4);
        assert_eq!(row_hit_end(text, 5, None), text.len());
        // Multi-byte character before the wrap point.
        let text = "canción largó";
        let next = "canción ".len();
        assert_eq!(row_hit_end(text, 0, Some(next)), "canción".len());
        // A one-character row never ends before it starts.
        assert_eq!(row_hit_end("ab", 1, Some(2)), 1);
    }

    #[test]
    fn backspace_removes_whole_grapheme() {
        let mut b = EditBuffer::new("canción 👍🏽");
        assert!(b.backspace());
        assert_eq!(b.text, "canción ");
        b.move_to("canci".len());
        b.delete();
        assert_eq!(b.text, "cancin ");
    }

    #[test]
    fn selection_reverses_when_crossing_anchor() {
        let mut b = EditBuffer::new("hola mundo");
        b.move_to(4);
        b.select_to(8);
        assert_eq!(b.selected(), " mun");
        b.select_to(1);
        assert_eq!(b.selected(), "ola");
        assert_eq!(b.cursor(), 1);
        b.replace(None, "OLA");
        assert_eq!(b.text, "hOLA mundo");
        assert_eq!(b.cursor(), 4);
    }

    #[test]
    fn word_and_line_motion() {
        let b = EditBuffer::new("uno dos\ntres cuatro");
        assert_eq!(b.prev_word(b.text.len()), "uno dos\ntres ".len());
        assert_eq!(b.next_word(0), 3);
        assert_eq!(b.line_start(10), 8);
        assert_eq!(b.line_end(2), 7);
    }

    #[test]
    fn utf16_round_trip_with_accents_and_emoji() {
        let b = EditBuffer::new("ñ😀a");
        let after_emoji = "ñ😀".len();
        assert_eq!(b.offset_to_utf16(after_emoji), 3);
        assert_eq!(b.offset_from_utf16(3), after_emoji);
    }

    #[test]
    fn nothing_to_delete_is_reported() {
        let mut b = EditBuffer::new("");
        assert!(!b.backspace());
        assert!(!b.delete());
    }
}
