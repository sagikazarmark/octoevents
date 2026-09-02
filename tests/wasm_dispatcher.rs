//! Compile-time coverage for non-`Send` wasm Dispatcher handlers.

#![cfg(target_arch = "wasm32")]

use std::{cell::Cell, rc::Rc};

use octoevents::{Dispatcher, EventKind};

#[test]
fn dispatcher_accepts_single_threaded_handler_state() {
    let calls = Rc::new(Cell::new(0));
    let handler_calls = Rc::clone(&calls);

    let _dispatcher = Dispatcher::<()>::builder()
        .on(EventKind::Push, move |_| {
            let calls = Rc::clone(&handler_calls);
            async move {
                calls.set(calls.get() + 1);
                Ok(())
            }
        })
        .build();
}
