//! Focused behaviour tests for the emulation core.

use super::attrs::{Attrs, Color};
use super::*;

fn term(cols: usize, rows: usize) -> Term {
    Term::new(cols, rows)
}

fn line(t: &Term, y: usize) -> String {
    t.screen().rows[y].text()
}

#[test]
fn print_and_pending_wrap() {
    let mut t = term(10, 4);
    t.feed(b"0123456789");
    assert_eq!(line(&t, 0), "0123456789");
    assert!(t.screen().pending_wrap, "10th column arms the wrap");
    assert!(t.screen().rows[0].wrapped);

    t.feed(b"a");
    assert_eq!(line(&t, 0), "0123456789");
    assert_eq!(line(&t, 1), "a");
    assert!(!t.screen().pending_wrap);
    assert_eq!(t.screen().cursor.x, 1);
    assert_eq!(t.screen().cursor.y, 1);
}

#[test]
fn pending_wrap_cancelled_by_cursor_motion() {
    let mut t = term(10, 3);
    t.feed(b"0123456789");
    assert!(t.screen().pending_wrap);
    t.feed(b"\x1b[1;3HX");
    assert_eq!(line(&t, 0), "01X3456789");
    assert_eq!(line(&t, 1), "");
}

#[test]
fn wide_char_at_right_margin_wraps() {
    let mut t = term(10, 3);
    t.feed("123456789漢".as_bytes());
    assert_eq!(line(&t, 0), "123456789");
    assert_eq!(line(&t, 1), "漢");
    assert_eq!(t.screen().rows[1].cells[0].ch, '漢');
    assert!(t.screen().rows[1].cells[1].cont);

    let mut t = term(10, 2);
    t.feed("12345678漢".as_bytes());
    assert_eq!(line(&t, 0), "12345678漢");
    assert!(t.screen().rows[0].cells[9].cont);
    assert!(t.screen().pending_wrap);
}

#[test]
fn overwriting_half_of_wide_char_clears_other_half() {
    let mut t = term(10, 2);
    t.feed("漢".as_bytes());
    t.feed(b"\x1b[1;2Hx");
    assert_eq!(line(&t, 0), " x");
    assert!(!t.screen().rows[0].cells[0].cont);
}

#[test]
fn linefeed_scroll_pushes_one_history_row() {
    let mut t = term(10, 4);
    t.feed(b"a\r\nb\r\nc\r\nd");
    assert_eq!(line(&t, 3), "d");
    t.feed(b"\n");
    assert_eq!(t.history().len(), 1);
    assert_eq!(t.history()[0].text(), "a");
    assert_eq!(line(&t, 0), "b");
    assert_eq!(line(&t, 1), "c");
    assert_eq!(line(&t, 2), "d");
    assert_eq!(line(&t, 3), "");
    assert!(t.screen().rows[3].cells.iter().all(|c| c.ch == ' '));
}

#[test]
fn lf_at_bottom_margin_does_not_touch_history() {
    let mut t = term(10, 4);
    t.feed(b"a\r\nb\r\nc\r\nd");
    assert_eq!(t.screen().cursor.y, 3);
    t.feed(b"\x1b[2;3r\x1b[3;1H\n");
    assert!(t.history().is_empty());
    assert_eq!(line(&t, 0), "a");
    assert_eq!(line(&t, 1), "c");
    assert_eq!(line(&t, 2), "");
    assert_eq!(line(&t, 3), "d");
}

#[test]
fn ed2_homes_and_blanks() {
    let mut t = term(10, 4);
    t.feed(b"hello\x1b[2;3H\x1b[2J");
    for y in 0..4 {
        assert_eq!(line(&t, y), "");
    }
    assert_eq!(t.screen().cursor.x, 0);
    assert_eq!(t.screen().cursor.y, 0);
}

#[test]
fn ed3_clears_history() {
    let mut t = term(10, 2);
    t.feed(b"a\r\nb\r\nc");
    assert_eq!(t.history().len(), 1);
    t.feed(b"\x1b[3J");
    assert!(t.history().is_empty());
    assert_eq!(line(&t, 0), "b");
}

#[test]
fn sgr_basic_and_simple_colours() {
    let mut t = term(10, 1);
    t.feed(b"\x1b[31;1mX");
    let cell = t.cell(0, 0).unwrap();
    assert_eq!(cell.fg, Color::Indexed(1));
    assert!(cell.attrs.has(Attrs::BOLD));

    t.feed(b"\x1b[0mY");
    let cell = t.cell(1, 0).unwrap();
    assert_eq!(cell.fg, Color::Default);
    assert!(!cell.attrs.has(Attrs::BOLD));
}

#[test]
fn sgr_extended_colours() {
    let mut t = term(10, 1);
    t.feed(b"\x1b[38;5;196mA");
    assert_eq!(t.cell(0, 0).unwrap().fg, Color::Indexed(196));
    t.feed(b"\x1b[48;5;21mB");
    assert_eq!(t.cell(1, 0).unwrap().bg, Color::Indexed(21));

    let mut t = term(10, 1);
    t.feed(b"\x1b[38;2;1;2;3mA");
    assert_eq!(t.cell(0, 0).unwrap().fg, Color::Rgb(1, 2, 3));

    let mut t = term(10, 1);
    t.feed(b"\x1b[38:2::4:5:6mA");
    assert_eq!(t.cell(0, 0).unwrap().fg, Color::Rgb(4, 5, 6));

    let mut t = term(10, 1);
    t.feed(b"\x1b[38:5:99mA");
    assert_eq!(t.cell(0, 0).unwrap().fg, Color::Indexed(99));
}

#[test]
fn sgr_attributes_toggle() {
    let mut t = term(20, 1);
    t.feed(b"\x1b[1;2;3;4;7;8;9mX");
    let cell = t.cell(0, 0).unwrap();
    for flag in [
        Attrs::BOLD,
        Attrs::DIM,
        Attrs::ITALIC,
        Attrs::UNDERLINE,
        Attrs::REVERSE,
        Attrs::HIDDEN,
        Attrs::STRIKE,
    ] {
        assert!(cell.attrs.has(flag), "missing {flag:?}");
    }
    t.feed(b"\x1b[22;23;24;27;28;29mY");
    let cell = t.cell(1, 0).unwrap();
    for flag in [
        Attrs::BOLD,
        Attrs::DIM,
        Attrs::ITALIC,
        Attrs::UNDERLINE,
        Attrs::REVERSE,
        Attrs::HIDDEN,
        Attrs::STRIKE,
    ] {
        assert!(!cell.attrs.has(flag), "still set {flag:?}");
    }
}

#[test]
fn alt_screen_1049_round_trip() {
    let mut t = term(10, 3);
    t.feed(b"main");
    assert_eq!(t.screen().cursor.x, 4);
    t.feed(b"\x1b[?1049h");
    assert!(t.alt_active());
    assert_eq!(line(&t, 0), "");
    assert_eq!(t.screen().cursor.x, 0);

    t.feed(b"alt");
    assert_eq!(line(&t, 0), "alt");

    t.feed(b"\x1b[?1049l");
    assert!(!t.alt_active());
    assert_eq!(line(&t, 0), "main");
    assert_eq!(t.screen().cursor.x, 4);
    assert_eq!(t.screen().cursor.y, 0);
}

#[test]
fn alt_47_keeps_content_and_does_not_save_cursor() {
    let mut t = term(10, 2);
    t.feed(b"x");
    t.feed(b"\x1b[?47h");
    assert!(t.alt_active());
    t.feed(b"alt");
    t.feed(b"\x1b[?47l");
    assert!(!t.alt_active());
    assert_eq!(line(&t, 0), "x");
}

#[test]
fn history_hidden_while_alt_active() {
    let mut t = term(10, 2);
    t.feed(b"a\r\nb\r\nc");
    assert_eq!(t.history().len(), 1);
    t.feed(b"\x1b[?1049h");
    assert!(t.history().is_empty());
    t.feed(b"\x1b[?1049l");
    assert_eq!(t.history().len(), 1);
}

#[test]
fn reverse_index_at_top_of_region() {
    let mut t = term(10, 4);
    t.feed(b"a\r\nb\r\nc\r\nd");
    t.feed(b"\x1b[2;4r\x1b[2;1H\x1bM");
    assert_eq!(line(&t, 0), "a");
    assert_eq!(line(&t, 1), "");
    assert_eq!(line(&t, 2), "b");
    assert_eq!(line(&t, 3), "c");
    assert!(t.history().is_empty());
}

#[test]
fn insert_and_delete_lines() {
    let mut t = term(10, 4);
    t.feed(b"a\r\nb\r\nc\r\nd");
    t.feed(b"\x1b[2;1H\x1b[1L");
    assert_eq!(line(&t, 0), "a");
    assert_eq!(line(&t, 1), "");
    assert_eq!(line(&t, 2), "b");
    assert_eq!(line(&t, 3), "c");

    let mut t = term(10, 4);
    t.feed(b"a\r\nb\r\nc\r\nd");
    t.feed(b"\x1b[2;1H\x1b[1M");
    assert_eq!(line(&t, 0), "a");
    assert_eq!(line(&t, 1), "c");
    assert_eq!(line(&t, 2), "d");
    assert_eq!(line(&t, 3), "");
}

#[test]
fn insert_delete_and_erase_characters() {
    let mut t = term(10, 2);
    t.feed(b"abcdef\x1b[1;2H\x1b[2@");
    assert_eq!(line(&t, 0), "a  bcdef");

    let mut t = term(10, 2);
    t.feed(b"abcdef\x1b[1;2H\x1b[2P");
    assert_eq!(line(&t, 0), "adef");

    let mut t = term(10, 2);
    t.feed(b"abcdef\x1b[1;2H\x1b[2X");
    assert_eq!(line(&t, 0), "a  def");
}

#[test]
fn erase_line_modes() {
    let mut t = term(10, 1);
    t.feed(b"abcdef\x1b[1;3H\x1b[K");
    assert_eq!(line(&t, 0), "ab");

    let mut t = term(10, 1);
    t.feed(b"abcdef\x1b[1;3H\x1b[1K");
    assert_eq!(line(&t, 0), "   def");

    let mut t = term(10, 1);
    t.feed(b"abcdef\x1b[1;2J");
    assert_eq!(line(&t, 0), "");
}

#[test]
fn esc7_esc8_save_and_restore_cursor_and_pen() {
    let mut t = term(10, 3);
    t.feed(b"ab\x1b7\x1b[31m\r\n\x1b8X");
    assert_eq!(line(&t, 0), "abX");
    assert_eq!(t.screen().cursor.x, 3);
    assert_eq!(t.screen().cursor.y, 0);
    assert_eq!(t.cell(2, 0).unwrap().fg, Color::Default);
}

#[test]
fn origin_mode_cup_is_region_relative() {
    let mut t = term(10, 6);
    t.feed(b"\x1b[2;4r\x1b[?6h\x1b[1;1H");
    assert!(t.screen().origin);
    assert_eq!(t.screen().cursor.y, 1);
    assert_eq!(t.screen().cursor.x, 0);

    t.feed(b"\x1b[2;1H");
    assert_eq!(t.screen().cursor.y, 2);

    t.feed(b"\x1b[?6l\x1b[1;1H");
    assert!(!t.screen().origin);
    assert_eq!(t.screen().cursor.y, 0);
}

#[test]
fn origin_mode_clamps_to_margins() {
    let mut t = term(10, 6);
    t.feed(b"\x1b[2;4r\x1b[?6h\x1b[99;1H");
    assert_eq!(t.screen().cursor.y, 3);
}

#[test]
fn resize_keeps_top_left_and_clamps_cursor() {
    let mut t = term(10, 4);
    t.feed(b"hello\r\nworld");
    assert_eq!(t.screen().cursor.x, 5);
    assert_eq!(t.screen().cursor.y, 1);

    t.resize(5, 2);
    assert_eq!(t.cols(), 5);
    assert_eq!(t.rows(), 2);
    assert_eq!(line(&t, 0), "hello");
    assert_eq!(line(&t, 1), "world");
    assert_eq!(t.screen().cursor.x, 4);
    assert_eq!(t.screen().cursor.y, 1);

    t.resize(3, 2);
    assert_eq!(line(&t, 0), "hel");
    assert_eq!(line(&t, 1), "wor");
    assert_eq!(t.screen().cursor.x, 2);
}

#[test]
fn tab_stops_and_cht() {
    let mut t = term(20, 1);
    t.feed(b"a\tb");
    assert_eq!(t.screen().rows[0].cells[8].ch, 'b');
    assert_eq!(t.screen().rows[0].cells[8].ch, 'b');

    let mut t = term(20, 1);
    t.feed(b"\x1b[2Ix");
    assert_eq!(t.screen().rows[0].cells[16].ch, 'x');

    let mut t = term(20, 1);
    t.feed(b"\x1b[1;9H\x1b[0g\x1b[1;1H\tx");
    assert_eq!(t.screen().rows[0].cells[16].ch, 'x');

    let mut t = term(20, 1);
    t.feed(b"\x1b[3g\x1b[1;1H\tx");
    assert_eq!(t.screen().rows[0].cells[19].ch, 'x');
}

#[test]
fn osc_sets_title() {
    let mut t = term(10, 2);
    t.feed(b"\x1b]0;my title\x07");
    assert_eq!(t.title(), "my title");
    t.feed(b"\x1b]2;two\x1b\\");
    assert_eq!(t.title(), "two");
    t.feed(b"\x1b]1;ignored\x07");
    assert_eq!(t.title(), "two");
}

#[test]
fn dsr_and_da_responses() {
    let mut t = term(10, 4);
    t.feed(b"\x1b[3;4H\x1b[6n");
    assert_eq!(t.take_responses(), b"\x1b[3;4R".to_vec());
    t.feed(b"\x1b[5n");
    assert_eq!(t.take_responses(), b"\x1b[0n".to_vec());
    t.feed(b"\x1b[c");
    assert_eq!(t.take_responses(), b"\x1b[?6c".to_vec());
}

#[test]
fn modes_bell_and_status_getters() {
    let mut t = term(10, 2);
    assert!(!t.app_cursor());
    assert!(!t.bracketed_paste());
    assert!(t.cursor_visible());
    t.feed(b"\x1b[?1h\x1b[?2004h\x1b[?25l\x07");
    assert!(t.app_cursor());
    assert!(t.bracketed_paste());
    assert!(!t.cursor_visible());
    assert!(t.take_bell());
    assert!(!t.take_bell());
}

#[test]
fn decscusr_styles() {
    let mut t = term(10, 2);
    t.feed(b"\x1b[3 q");
    assert_eq!(t.cursor_style(), CursorStyle::Underline);
    assert!(!t.cursor_blink());
    t.feed(b"\x1b[6 q");
    assert_eq!(t.cursor_style(), CursorStyle::BarBlink);
    assert!(t.cursor_blink());
    t.feed(b"\x1b[1 q");
    assert_eq!(t.cursor_style(), CursorStyle::Block);
}

#[test]
fn dec_special_graphics_charset() {
    let mut t = term(10, 1);
    t.feed(b"\x1b(0lqk\x1b(B");
    assert_eq!(line(&t, 0), "┌─┐");

    let mut t = term(10, 1);
    t.feed(b"\x1b)0\x0eq\x0fq");
    assert_eq!(line(&t, 0), "─q");
}

#[test]
fn insert_mode_shifts_row() {
    let mut t = term(10, 1);
    t.feed(b"abc\x1b[1;1H\x1b[?4hX");
    assert_eq!(line(&t, 0), "Xabc");
    t.feed(b"\x1b[?4lY");
    assert_eq!(line(&t, 0), "XYbc");
}

#[test]
fn soft_reset_keeps_grid_but_resets_modes() {
    let mut t = term(10, 3);
    t.feed(b"\x1b[31;1mabc\x1b[?6h\x1b[2;3r");
    assert!(t.screen().origin);
    t.feed(b"\x1b[!p");
    assert!(!t.screen().origin);
    assert_eq!(t.screen().top, 0);
    assert_eq!(t.screen().bottom, 2);
    assert_eq!(line(&t, 0), "abc");
    t.feed(b"X");
    assert_eq!(line(&t, 0), "Xbc");
    assert_eq!(t.cell(0, 0).unwrap().fg, Color::Default);
}

#[test]
fn erase_fills_use_pen_background() {
    let mut t = term(10, 2);
    t.feed(b"\x1b[44m\x1b[2J");
    assert_eq!(t.cell(0, 0).unwrap().bg, Color::Indexed(4));
    t.feed(b"\x1b[1;1H\x1b[K");
    assert_eq!(t.cell(0, 0).unwrap().bg, Color::Indexed(4));
}

#[test]
fn osc7_sets_cwd_and_survives_alt_screen() {
    let mut t = term(10, 2);
    assert_eq!(t.cwd(), None);
    t.feed(b"\x1b]7;file://host/home/ali/tessera\x07");
    assert_eq!(t.cwd(), Some("/home/ali/tessera"));

    t.feed(b"\x1b[?1049h");
    assert_eq!(t.cwd(), Some("/home/ali/tessera"));
    t.feed(b"\x1b[?1049l");
    assert_eq!(t.cwd(), Some("/home/ali/tessera"));

    // An empty path clears the tracked directory.
    t.feed(b"\x1b]7;file://host\x07");
    assert_eq!(t.cwd(), None);
}

#[test]
fn osc7_unparseable_payload_does_not_panic() {
    let mut t = term(10, 2);
    t.feed(b"\x1b]7;not-a-url\x07");
    assert_eq!(t.cwd(), None);
    t.feed(b"\x1b]7;\xff\xfe\x07");
    assert_eq!(t.cwd(), None);
    t.feed(b"\x1b]7;file://host/ok\x07");
    assert_eq!(t.cwd(), Some("/ok"));
    // OSC 7 with no payload keeps the previous value.
    t.feed(b"\x1b]7\x07");
    assert_eq!(t.cwd(), Some("/ok"));
}

#[test]
fn line_spans_history_and_screen() {
    let mut t = term(10, 3);
    t.feed(b"a\r\nb\r\nc\r\nd\r\ne");
    assert_eq!(t.history_len(), 2);
    assert_eq!(t.line(0).unwrap().text(), "a");
    assert_eq!(t.line(1).unwrap().text(), "b");
    assert_eq!(t.line(2).unwrap().text(), "c");
    assert_eq!(t.line(3).unwrap().text(), "d");
    assert_eq!(t.line(4).unwrap().text(), "e");
    assert!(t.line(5).is_none());

    // While the alternate screen is active there is no history, so `line`
    // addresses the alternate grid.
    t.feed(b"\x1b[?1049h");
    assert_eq!(t.history_len(), 0);
    assert_eq!(t.line(0).unwrap().text(), "");
    t.feed(b"\x1b[?1049l");
    assert_eq!(t.line(0).unwrap().text(), "a");
}

#[test]
fn history_len_tracks_scrollback_eviction() {
    let mut t = term(10, 2);
    assert_eq!(t.history_len(), 0);
    t.feed(b"1\r\n2\r\n3\r\n4");
    assert_eq!(t.history_len(), 2);
    assert_eq!(t.line(0).unwrap().text(), "1");
    assert_eq!(t.line(2).unwrap().text(), "3");
    assert_eq!(t.history().len(), 2);
}

#[test]
fn row_text_skips_wide_char_continuation() {
    let mut t = term(6, 1);
    t.feed("漢字".as_bytes());
    let row = &t.screen().rows[0];
    assert_eq!(row.text(), "漢字");
    assert_eq!(row.text().chars().count(), 2);
    assert!(row.cells[1].cont);
    assert!(row.cells[3].cont);
    assert_eq!(row.cells[1].ch, ' ');
}
