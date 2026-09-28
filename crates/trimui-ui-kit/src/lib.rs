//! Reusable UI controls for TrimUI handheld applications.
//!
//! The keyboard owns its layout and controller navigation state, but never
//! owns an application's text buffer or submit behavior. Consumers receive
//! [`KeyboardAction`] values and decide how to apply them.

pub mod keyboard;
