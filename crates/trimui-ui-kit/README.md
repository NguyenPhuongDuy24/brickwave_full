# trimui-ui-kit

Reusable egui controls for TrimUI applications. The first component is a
bottom-anchored virtual keyboard designed for a 1024×768 screen.

The library owns keyboard layout, Shift/Symbol state, D-pad selection and
button repeat. It deliberately does not own an application's text buffer,
search action, login action or backend.

## Controller contract

- D-pad moves the highlighted key. Up/Down selects the nearest key center in
  the adjacent row.
- A maps to `KeyboardControl::Primary`; one action is emitted on release.
- B maps to `KeyboardControl::Back`; it closes the keyboard and is consumed.
- When `handle_control` returns `true`, the host must not also scroll, click or
  run an application shortcut for that input.

## Minimal integration

```rust
use trimui_ui_kit::keyboard::{
    KeyboardAction, KeyboardConfig, KeyboardControl, VirtualKeyboard,
};

let mut keyboard = VirtualKeyboard::default();
keyboard.open(KeyboardConfig::search());

// Handheld event loop:
if keyboard.handle_control(KeyboardControl::DpadX(1)) {
    // Do not forward this event to page scrolling.
}

// At the beginning of the egui frame, consume `take_actions()` and apply
// Insert/Backspace to the focused TextEdit. The application owns Clear,
// Submit and Close behavior. At the end of the frame:
keyboard.show(ctx);
```

Arrow keys and Enter provide the equivalent navigation in a Windows preview.
The consumer should keep the target `TextEdit` focused while the keyboard is
open and surrender focus after Submit or Close.
