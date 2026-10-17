//! wxlike desktop entry point. All app logic lives in the library so the
//! command layer can be integration-tested against a real server.

// the MSVC linker prints localized progress lines on non-English Windows
#![allow(linker_messages)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    wxlike_ui_lib::run()
}
