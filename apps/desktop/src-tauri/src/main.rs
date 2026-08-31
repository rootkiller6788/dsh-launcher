// Hide the console window in release builds (PCL-like: no terminal at all).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    ai_harness_launcher_lib::run()
}
