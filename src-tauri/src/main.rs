// no console window on win in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    safai_lib::run()
}
