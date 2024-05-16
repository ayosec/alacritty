use crate::event::EventListener;
use crate::index::{Column, Line, Point};
use crate::term::cell::Flags;
use crate::Term;

pub fn osc_execute<T: EventListener>(term: &mut Term<T>, params: &[&[u8]]) {
    // params[1]:
    //      0: jump to the previous bookmark.
    //      1: add bookmark flag for the next characters
    //      2: stop adding bookmark flag.
    match params.get(1) {
        Some([b'0']) => jump_bookmark(term),
        Some([b'1']) => start_bookmark(term),
        Some([b'2']) => finish_bookmark(term),
        _ => (),
    }
}

#[inline]
fn start_bookmark<T: EventListener>(term: &mut Term<T>) {
    term.grid.cursor.template.flags.insert(Flags::BOOKMARK);
}

#[inline]
fn finish_bookmark<T: EventListener>(term: &mut Term<T>) {
    term.grid.cursor.template.flags.remove(Flags::BOOKMARK);
}

#[inline]
fn jump_bookmark<T: EventListener>(term: &mut Term<T>) {
    // Check if the visible grid contains any bookmark before the line at
    // the cursor.

    let point = (0..term.grid.cursor.point.line.0)
        .rev()
        .find_map(|line| term.grid[Line(line)].bookmark().map(|c| Point::new(Line(line), c)))
        .unwrap_or_else(|| Point::new(Line(0), Column(0)));

    term.grid.cursor.point = point;
}
